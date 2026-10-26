#!/bin/sh
# From the repository root: sh .ui-browser/tests/stream-cleanup.sh
# Requires the existing fixture image and Docker Compose 2.24.4+ (!override).
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
project="openproxy-ui-sse-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"

compose() {
  docker compose --project-name "$project" --project-directory "$root/.ui-browser" \
    --env-file /dev/null -f "$root/.ui-browser/compose.yaml" \
    -f "$root/.ui-browser/tests/stream-cleanup.compose.yaml" "$@"
}

# Never adopt or remove an existing project, even in the unlikely event of a name collision.
if [ -n "$(compose ps -aq)" ] || [ -n "$(docker network ls -q --filter "name=^${project}_isolated$")" ]; then
  echo "Refusing to use existing project: $project" >&2
  exit 1
fi

cleanup() {
  status=$?
  trap - EXIT
  if ! compose down; then
    echo "Cleanup failed for project: $project" >&2
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

echo "Synthetic Compose project: $project"
compose up -d --no-deps --pull never --no-build backend app
compose run --rm --no-deps --pull never -T --entrypoint node browser --test /tests/stream-cleanup.test.mjs
