#!/bin/bash
# git-branch — create a writable working copy from a read-only mounted repo.
#
# Usage:  git-branch <org/repo> [branch slug]
# Example: git-branch owner/centaur fix-flaky-slack-delivery
#
# Creates ~/branches/<org>/<repo> as a --shared clone from ~/github/<org>/<repo>
# with a unique agent branch checked out. The resulting directory is fully writable
# and supports commit, push, and PR workflows.

set -euo pipefail

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
    echo "Usage: git-branch <org/repo> [branch slug]" >&2
    exit 1
fi

REPO="$1"
SLUG="${2:-}"
SRC="$HOME/github/$REPO"
DEST="$HOME/branches/$REPO"

if [ ! -d "$SRC/.git" ] && ! git -C "$SRC" rev-parse --git-dir >/dev/null 2>&1; then
    echo "Error: $SRC is not a valid git repository" >&2
    exit 1
fi

if [ -d "$DEST/.git" ]; then
    echo "$DEST already exists — reusing" >&2
    echo "$DEST"
    exit 0
fi

mkdir -p "$(dirname "$DEST")"

if ! git clone --quiet --shared "$SRC" "$DEST"; then
    echo "shared clone failed; retrying with regular clone" >&2
    rm -rf "$DEST"
    git clone --quiet "$SRC" "$DEST"
fi

# --shared clones set origin to the local path; fix it to the upstream URL
# so that git push and gh pr create target the real GitHub remote.
UPSTREAM_URL=$(git -C "$SRC" config --get remote.origin.url 2>/dev/null || echo "")
if [ -n "$UPSTREAM_URL" ]; then
    git -C "$DEST" remote set-url origin "$UPSTREAM_URL"
fi

if [ -n "$SLUG" ]; then
    BRANCH="centaur/$SLUG-$(date +%s)"
else
    BRANCH="centaur/$(date +%s)-${RANDOM}-${RANDOM}"
fi
git -C "$DEST" checkout -q -b "$BRANCH"

echo "$DEST"
