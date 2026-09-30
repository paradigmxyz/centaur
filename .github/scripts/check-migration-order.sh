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

# check_migrations <label> <regex> <primary-dir> [variant-dir...]
#
# All directories share one version sequence. A version may appear once in the
# primary directory, or once in every variant directory (variants are mutually
# exclusive alternatives, such as text-search backends, so each must carry every
# variant version). Every added file must have a version greater than any
# version on the base ref.
check_migrations() {
  local label="$1"
  local regex="$2"
  shift 2
  local dirs=("$@")
  local dir_failed=0

  local head_entries=""
  local base_entries=""
  local index
  for index in "${!dirs[@]}"; do
    local dir="${dirs[${index}]}"
    if [[ -d "${dir}" ]]; then
      head_entries+="$(
        find "${dir}" -maxdepth 1 -type f -print | sort | extract_versions "${regex}" \
          | awk -v dir_index="${index}" '{ print $1, dir_index, $2 }'
      )"$'\n'
    fi
    base_entries+="$(
      git ls-tree -r --name-only "${base_ref}" -- "${dir}" | sort | extract_versions "${regex}" \
        | awk -v dir_index="${index}" '{ print $1, dir_index, $2 }'
    )"$'\n'
  done

  local duplicate_versions
  duplicate_versions="$(
    printf '%s' "${head_entries}" | awk '
      NF {
        version = $1 + 0
        seen[version]++
        per_dir[version, $2]++
        if ($2 == 0) primary[version]++
      }
      END {
        for (key in per_dir) {
          if (per_dir[key] > 1) {
            split(key, parts, SUBSEP)
            conflict[parts[1]] = 1
          }
        }
        for (version in primary) {
          if (seen[version] > primary[version]) conflict[version] = 1
        }
        for (version in conflict) print version
      }
    ' | sort -n
  )"

  if [[ -n "${duplicate_versions}" ]]; then
    failed=1
    dir_failed=1
    echo "::error title=${label} duplicate migration versions::Migration versions must be unique, except across variant directories: ${dirs[*]}"
    while IFS= read -r version; do
      [[ -n "${version}" ]] || continue
      echo "  ${version}:"
      printf '%s' "${head_entries}" | awk -v version="${version}" 'NF && $1 + 0 == version { print "    " $3 }'
    done <<<"${duplicate_versions}"
  fi

  local variant_count=$(( ${#dirs[@]} - 1 ))
  if (( variant_count > 1 )); then
    local unpaired_versions
    unpaired_versions="$(
      printf '%s' "${head_entries}" | awk -v variants="${variant_count}" '
        NF && $2 != 0 { dirs[$1 + 0, $2] = 1; versions[$1 + 0] = 1 }
        END {
          for (version in versions) {
            for (dir = 1; dir <= variants; dir++) {
              if (!((version, dir) in dirs)) { print version; break }
            }
          }
        }
      ' | sort -n
    )"
    if [[ -n "${unpaired_versions}" ]]; then
      failed=1
      dir_failed=1
      echo "::error title=${label} unpaired variant migrations::Every variant directory needs each variant version (add a no-op migration where nothing changes): ${dirs[*]:1}"
      while IFS= read -r version; do
        [[ -n "${version}" ]] || continue
        echo "  ${version}:"
        printf '%s' "${head_entries}" | awk -v version="${version}" 'NF && $1 + 0 == version { print "    " $3 }'
      done <<<"${unpaired_versions}"
    fi
  fi

  local base_max
  base_max="$(
    printf '%s' "${base_entries}" | awk 'NF { print $1 + 0 }' | sort -n | tail -n 1
  )"

  if [[ -z "${base_max}" ]]; then
    echo "${label}: no base migrations found under ${dirs[*]}; skipping monotonic version check."
    return
  fi

  local added_entries
  added_entries="$(
    comm -23 \
      <(printf '%s' "${head_entries}" | awk 'NF { print $3, $1 }' | sort -u) \
      <(printf '%s' "${base_entries}" | awk 'NF { print $3, $1 }' | sort -u)
  )"

  while IFS=' ' read -r path version; do
    [[ -n "${path}" ]] || continue
    if (( $(version_number "${version}") <= base_max )); then
      failed=1
      dir_failed=1
      echo "::error title=${label} non-monotonic migration::New migration ${path} must have a version greater than ${base_max} from ${base_ref}"
    fi
  done <<<"${added_entries}"

  if [[ "${dir_failed}" -eq 0 ]]; then
    echo "${label}: migration versions are unique$( (( variant_count > 1 )) && echo ', paired,' ) and monotonic relative to ${base_ref}."
  fi
}

sqlx_crate="services/api-rs/crates/centaur-session-sqlx"
check_migrations \
  "SQLx" \
  '^([0-9]+)_.+\.sql$' \
  "${sqlx_crate}/migrations" \
  "${sqlx_crate}/search-migrations/paradedb" \
  "${sqlx_crate}/search-migrations/postgres"

check_migrations \
  "Rails console" \
  '^([0-9]+)_.+\.rb$' \
  "services/console/db/migrate"

exit "${failed}"
