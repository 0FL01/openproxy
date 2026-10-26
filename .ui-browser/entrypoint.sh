#!/bin/sh
set -eu

# Chrome on Linux consults the per-user NSS shared database for local roots.
# The CA only signs the synthetic, internal "app" fixture certificate.
db="$HOME/.pki/nssdb"
mkdir -p "$db"
certutil -N -d "sql:$db" --empty-password
certutil -A -d "sql:$db" -n ui-browser-fixture-ca -t 'C,,' \
  -i /opt/ui-browser/tls/ca.crt
exec "$@"
