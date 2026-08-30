#!/bin/sh
set -eu

: "${DATABASE_URL:?pglite-server must supply DATABASE_URL}"
export HEARTBEAT_TEST_DATABASE_URL="$DATABASE_URL"
uv run python -m unittest tests.test_heartbeat_postgres
