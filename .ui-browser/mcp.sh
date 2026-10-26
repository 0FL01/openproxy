#!/bin/sh
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec docker compose --project-name openproxy-ui-browser-r1 --project-directory "$here" \
  -f "$here/compose.yaml" run --rm --no-deps --pull never -T browser
