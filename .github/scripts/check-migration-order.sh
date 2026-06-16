#!/usr/bin/env bash
set -euo pipefail

base_ref="${1:-origin/${GITHUB_BASE_REF:-main}}"
failed=0

if ! git rev-parse --verify --quiet "${base_ref}^{commit}" >/dev/null; then
  echo "Base ref '${base_ref}' does not exist. Fetch it before running this check." >&2
  exit 1
fi

extract_versions() {
  local regex="$1"
  while IFS= read -r path; do
    local file="${path##*/}"
    if [[ "${file}" =~ ${regex} ]]; then
      printf '%s %s\n' "${BASH_REMATCH[1]}" "${path}"
    fi
  done
}

version_number() {
  local version="$1"
  echo $((10#${version}))
}

check_migrations() {
  local label="$1"
  local dir="$2"
  local regex="$3"
  local dir_failed=0

  local head_entries
  head_entries="$(
    find "${dir}" -maxdepth 1 -type f -print | sort | extract_versions "${regex}"
  )"

  local duplicate_versions
  duplicate_versions="$(
    printf '%s\n' "${head_entries}" | awk 'NF { print $1 }' | sort | uniq -d
  )"

  if [[ -n "${duplicate_versions}" ]]; then
    failed=1
    dir_failed=1
    echo "::error title=${label} duplicate migration versions::Duplicate migration version prefixes found in ${dir}"
    while IFS= read -r version; do
      [[ -n "${version}" ]] || continue
      echo "  ${version}:"
      printf '%s\n' "${head_entries}" | awk -v version="${version}" '$1 == version { print "    " $2 }'
    done <<<"${duplicate_versions}"
  fi

  local base_entries
  base_entries="$(
    git ls-tree -r --name-only "${base_ref}" -- "${dir}" | sort | extract_versions "${regex}"
  )"

  local base_max
  base_max="$(
    printf '%s\n' "${base_entries}" | awk 'NF { print $1 }' | sort -n | tail -n 1
  )"

  if [[ -z "${base_max}" ]]; then
    echo "${label}: no base migrations found under ${dir}; skipping monotonic version check."
    return
  fi

  local added_versions
  added_versions="$(
    comm -23 \
      <(printf '%s\n' "${head_entries}" | awk 'NF { print $1 }' | sort -u) \
      <(printf '%s\n' "${base_entries}" | awk 'NF { print $1 }' | sort -u)
  )"

  while IFS= read -r version; do
    [[ -n "${version}" ]] || continue
    if (( $(version_number "${version}") <= $(version_number "${base_max}") )); then
      failed=1
      dir_failed=1
      echo "::error title=${label} non-monotonic migration::New migration version ${version} must be greater than ${base_max} from ${base_ref}"
      printf '%s\n' "${head_entries}" | awk -v version="${version}" '$1 == version { print "  " $2 }'
    fi
  done <<<"${added_versions}"

  if [[ "${dir_failed}" -eq 0 ]]; then
    echo "${label}: migration versions are monotonic relative to ${base_ref}."
  fi
}

check_migrations \
  "SQLx" \
  "services/api-rs/crates/centaur-session-sqlx/migrations" \
  '^([0-9]+)_.+\.sql$'

check_migrations \
  "Rails console" \
  "services/console/db/migrate" \
  '^([0-9]+)_.+\.rb$'

exit "${failed}"
