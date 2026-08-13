#!/bin/sh
set -eu

HOME_DIR="${1:?home directory is required}"
STATE_DIR="${2:?state directory is required}"
PERSISTENT_STATE="${3:-0}"

if [ -n "${CENTAUR_WORKSPACE_ROOT:-}" ]; then
    case "$CENTAUR_WORKSPACE_ROOT" in
        /*) printf '%s\n' "$CENTAUR_WORKSPACE_ROOT" ;;
        *)
            echo "CENTAUR_WORKSPACE_ROOT must be absolute" >&2
            exit 1
            ;;
    esac
elif [ "$PERSISTENT_STATE" = "1" ]; then
    printf '%s/workspace\n' "$STATE_DIR"
else
    printf '%s/workspace\n' "$HOME_DIR"
fi
