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

    test_pythonpath="${test_package_dir}:${repo_root}"
    # Keep legacy folder imports unless they would shadow an installed dependency.
    if ! uv run --no-sync python -c \
      'import importlib.metadata, sys; sys.exit(sys.argv[1] not in {d.metadata["Name"] for d in importlib.metadata.distributions()})' \
      "${tool_dir##*/}"; then
      test_pythonpath="${test_pythonpath}:${tool_dir%/*}"
    fi

    while IFS= read -r test_file; do
      relative_test="${test_file#"${tool_dir}/"}"
      PYTHONPATH="${test_pythonpath}" \
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
