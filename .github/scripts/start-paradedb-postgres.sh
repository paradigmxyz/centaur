#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "usage: $0 <container-name> <host-port> <database>" >&2
  exit 64
fi

container_name="$1"
host_port="$2"
database="$3"

docker run --detach --name "${container_name}" \
  --publish "127.0.0.1:${host_port}:5432" \
  --env POSTGRES_USER=postgres \
  --env POSTGRES_PASSWORD=postgres \
  --env "POSTGRES_DB=${database}" \
  paradedb/paradedb:0.23.0-pg16 \
  -c shared_preload_libraries=pg_search,pg_cron \
  -c max_connections=500

for attempt in {1..60}; do
  if docker logs "${container_name}" 2>&1 \
    | grep -q 'PostgreSQL init process complete' \
    && docker exec "${container_name}" \
    psql -U postgres -d "${database}" -tAc 'select 1' >/dev/null; then
    exit 0
  fi
  sleep 1
done

docker logs "${container_name}"
exit 1
