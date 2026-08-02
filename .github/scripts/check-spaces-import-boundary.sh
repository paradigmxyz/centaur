#!/usr/bin/env bash
# Enforce the Spaces portability boundary: code under spaces/ must not import
# Centaur packages. The adapter may encode Centaur HTTP shapes, but even it must
# not depend on Centaur SDKs — swap the runtime by rewriting the adapter alone.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
spaces_dir="${root}/spaces"

if [[ ! -d "${spaces_dir}" ]]; then
  echo "spaces/ directory missing; nothing to check."
  exit 0
fi

# Source-like files only. Docs may describe Centaur freely.
mapfile -t files < <(
  find "${spaces_dir}" -type f \
    \( -name '*.py' -o -name '*.ts' -o -name '*.tsx' -o -name '*.js' -o -name '*.jsx' \
       -o -name '*.rb' -o -name '*.rs' -o -name '*.go' \) \
    ! -path '*/.*' \
    | sort
)

if [[ ${#files[@]} -eq 0 ]]; then
  echo "No Spaces source files to check."
  exit 0
fi

patterns=(
  'centaur_sdk'
  'centaur-sdk'
  'from centaur'
  'import centaur'
  '@centaur/'
  'services/api-rs'
  'services/console'
  'packages/centaur'
)

failed=0
for file in "${files[@]}"; do
  rel="${file#"${root}/"}"
  for pattern in "${patterns[@]}"; do
    if grep -nE "${pattern}" "${file}" >/dev/null 2>&1; then
      echo "Import boundary violation in ${rel} (matched /${pattern}/):" >&2
      grep -nE "${pattern}" "${file}" >&2 || true
      failed=1
    fi
  done
done

if [[ "${failed}" -ne 0 ]]; then
  echo >&2
  echo "Spaces code must not import Centaur packages." >&2
  echo "Talk to Centaur only through spaces/adapter HTTP shapes, or via the" >&2
  echo "console OAuth strategy files under services/console/lib/oauth/providers/." >&2
  exit 1
fi

echo "Spaces import boundary OK (${#files[@]} file(s) checked)."
