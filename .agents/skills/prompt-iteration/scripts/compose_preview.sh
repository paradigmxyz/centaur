#!/usr/bin/env bash
# Compose a sandbox system prompt from edited persona/overlay/base files, the
# same way sandbox startup does, into OUT_DIR/AGENTS.md.
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage: compose_preview.sh [--base FILE] [--persona FILE | --no-persona] [--repo DIR]... OUT_DIR

  --base FILE     Use FILE as the base prompt instead of this sandbox's base.
  --persona FILE  Use FILE as the persona prompt (default: this session's persona).
  --no-persona    Compose without a persona.
  --repo DIR      A checkout at .../<org>/<repo> whose services/sandbox/SYSTEM_PROMPT.md
                  replaces ~/github/<org>/<repo> as an overlay. Repeatable.
EOF
    exit 2
}

base="" persona="" no_persona=0 repos=()
while [ $# -gt 1 ]; do
    case "$1" in
        --base) base="$2"; shift 2 ;;
        --persona) persona="$2"; shift 2 ;;
        --no-persona) no_persona=1; shift ;;
        --repo) repos+=("$2"); shift 2 ;;
        *) usage ;;
    esac
done
[ $# -eq 1 ] || usage
out="$1"

home="$out/home"
mount="$out/github"
rm -rf "$home" "$mount" "$out/AGENTS.md" "$out/.agents" "$out/.claude"
mkdir -p "$home" "$mount"

copy_if_file() { if [ -f "$1" ]; then cp "$1" "$2"; fi; }

if [ -n "$base" ]; then
    # Both names: AGENTS_BASE.md is the selected base, and AGENTS.md lets
    # compose drop a mounted checkout whose SYSTEM_PROMPT.md equals the base.
    cp "$base" "$home/AGENTS_BASE.md"
    cp "$base" "$home/AGENTS.md"
else
    copy_if_file "$HOME/AGENTS_BASE.md" "$home/AGENTS_BASE.md"
    copy_if_file "$HOME/AGENTS.md" "$home/AGENTS.md"
fi
copy_if_file "$HOME/AGENTS_OVERLAY.md" "$home/AGENTS_OVERLAY.md"

if [ -n "$persona" ]; then
    cp "$persona" "$home/AGENTS_PERSONA.md"
elif [ "$no_persona" = 0 ]; then
    copy_if_file "$HOME/AGENTS_PERSONA.md" "$home/AGENTS_PERSONA.md"
fi

link_repo() {
    local src org_dir
    src="$(cd "$1" && pwd)"
    # With --base, a checkout still carrying the baked base is the old base,
    # not an overlay; compose only dedupes against the new one.
    if [ -n "$base" ] && [ -f "$HOME/AGENTS.md" ] \
        && cmp -s "$src/services/sandbox/SYSTEM_PROMPT.md" "$HOME/AGENTS.md"; then
        return
    fi
    org_dir="$mount/$(basename "$(dirname "$src")")"
    mkdir -p "$org_dir"
    ln -sfn "$src" "$org_dir/$(basename "$src")"
}
for repo in "$HOME"/github/*/*; do
    if [ -d "$repo" ]; then link_repo "$repo"; fi
done
for repo in ${repos[@]+"${repos[@]}"}; do
    link_repo "$repo"
done

# Expose the session's installed skills, found the same way as
# install_tool_shims._workspace_dir(), to children started in OUT_DIR.
workspace="${WORKSPACE_DIR:-${CENTAUR_WORKSPACE_DIR:-}}"
if [ -z "$workspace" ]; then
    workspace="${CENTAUR_STATE_DIR:-$HOME/state}/workspace"
    if [ "${CENTAUR_PERSISTENT_STATE:-}" != 1 ] && [ ! -e "$workspace" ]; then
        workspace="$HOME/workspace"
    fi
fi
if [ -d "$workspace/.agents/skills" ]; then
    mkdir -p "$out/.agents" "$out/.claude"
    ln -sfn "$workspace/.agents/skills" "$out/.agents/skills"
    ln -sfn "$workspace/.agents/skills" "$out/.claude/skills"
else
    echo "warning: no skills at $workspace/.agents/skills; the child will lack session skills" >&2
fi

compose-system-prompt --home-dir "$home" --repo-mount "$mount" --target-prompt "$out/AGENTS.md"
[ -f "$out/AGENTS.md" ] || { echo "no base prompt found" >&2; exit 1; }
echo "$out/AGENTS.md"
