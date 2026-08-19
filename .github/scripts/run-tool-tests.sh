#!/usr/bin/env bash

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
tools_dir="${repo_root}/tools"

while IFS= read -r project_file; do
  tool_dir="${project_file%/pyproject.toml}"
  test_files="$(
    find "${tool_dir}" \
      -path '*/.venv' -prune -o \
      -type f -name 'test_*.py' -print \
      | sort
  )"
  if [[ -z "${test_files}" ]]; then
    continue
  fi

  echo "Testing ${tool_dir#"${repo_root}/"}"
  (
    cd "${tool_dir}"
    uv sync --no-install-project
    uv pip install pytest

    test_package_dir="$(mktemp -d)"
    trap 'rm -rf "${test_package_dir}"' EXIT
    script_module="$(
      uv run --no-sync python -c \
        'import tomllib; data = tomllib.load(open("pyproject.toml", "rb")); print(next(iter(data["project"]["scripts"].values())).split(".", 1)[0])'
    )"
    ln -s "${tool_dir}" "${test_package_dir}/${script_module}"

    while IFS= read -r test_file; do
      relative_test="${test_file#"${tool_dir}/"}"
      PYTHONPATH="${test_package_dir}:${repo_root}:${tool_dir%/*}" \
        uv run --no-sync python -m pytest \
          --import-mode=importlib \
          "${relative_test}"
    done <<<"${test_files}"
  )
done < <(
  find "${tools_dir}" \
    -path '*/.venv' -prune -o \
    -name pyproject.toml -print \
    | sort
)
