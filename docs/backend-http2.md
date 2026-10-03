# Backend HTTP/2 deployment

The backend chain is `nginx → h2c → OpenProxy → TLS/ALPN h2 → provider`.
Client HTTP/1.1 remains supported. No OpenCode/client configuration or nftables
changes are required. Reqwest negotiates HTTP/2 rather than forcing it, so
HTTP/1.1-only providers and configured proxies retain their existing behavior.

## nginx

Ordinary `proxy_pass` HTTP/2 requires nginx **1.29.4 or newer**, built with
`--with-http_v2_module`. Use a patched stable release (this rollout: 1.30.5).
Keep the existing loopback upstream, TLS, forwarding headers and authentication
locations. In the **OpenProxy-only** proxy snippet:

```nginx
proxy_pass http://openproxy_prod;
proxy_http_version 2;
proxy_set_header Upgrade "";
proxy_set_header Connection "";

proxy_set_header Host $host;
proxy_set_header X-Real-IP $remote_addr;
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
proxy_set_header X-Forwarded-Proto https;
proxy_set_header X-Forwarded-Host $host;
proxy_set_header X-Forwarded-Port 443;
proxy_set_header X-Request-ID $request_id;

proxy_buffering off;
proxy_request_buffering on;
proxy_cache off;
proxy_connect_timeout 60s;
proxy_send_timeout 3600s;
proxy_read_timeout 3600s;
```

Do not reuse the global WebSocket `Connection: upgrade/close` map on this hop.
Do not change independent Authelia subrequests or other services' snippets.
Validate with `nginx -t` before `systemctl reload nginx`.

### Debian package transition warning

The nginx.org stable repository supports Debian 13/trixie. Verify its signing
key against the official documentation, pin only nginx packages, and simulate
the transaction first. Migrating from Debian's `nginx`/`nginx-common` packaging
can **stop nginx, replace nginx.conf, and add conf.d/default.conf**, even with
`--force-confold`. Installation success is not service availability.

Privately back up the binary, service unit and full configuration; retain the
old packages and backend image. Stage and test the new binary against existing
configuration before the maintenance window. Immediately restore the original
nginx.conf/includes if replaced, disable any newly introduced default vhost,
run `nginx -t`, and explicitly start the service. Verify `systemctl is-active
nginx`, the running master executable, listening ports, all existing vhosts,
and the API. A reload alone does not upgrade an already-running master binary.
Do not leave a temporary systemd drain override installed afterward.

## Backend deployment and evidence

Preserve `.env.prod`, encryption material and the named `openproxy-prod-data`
volume. Never use `docker compose down --volumes` for an upgrade.

```bash
OPENPROXY_BUILD_COMMIT="$(git rev-parse HEAD)" docker compose build openproxy
docker compose up -d --no-build --wait openproxy
curl --http2-prior-knowledge -fsS http://127.0.0.1:4623/health
```

At INFO, target `openproxy::transport` reports actual versions without URLs,
credentials, accounts, model names, headers or bodies:

```text
HTTP transport leg="inbound" http_version=HTTP/2.0
HTTP transport leg="upstream" executor="codex" transport="reqwest" http_version=HTTP/2.0 status=200
HTTP transport leg="upstream" executor="opencode" transport="reqwest" http_version=HTTP/2.0 status=200
HTTP transport leg="upstream" executor="default" transport="hyper" http_version=HTTP/2.0 status=200
```

Use authenticated observability logs to inspect these events; the server's
console buffer is not necessarily Docker stdout. Only application ingress
version proves nginx's upstream protocol. For each configured provider, verify
a small authenticated JSON and live SSE generation, usage, and successful H2
upstream response. Never publish raw credential-bearing logs. Deterministic
tests `transport_tests` and `backend_http2` cover verified TLS ALPN, H1 fallback,
pool reuse, live tools/usage, cancellation, read timeouts and ingress auth.

For independent rollback: set the OpenProxy snippet back to
`proxy_http_version 1.1` with `Connection ""` and `Upgrade ""`, then test/reload;
retag the retained backend image and recreate only OpenProxy without removing
its volume; or restore the old nginx package/binary, unit and configuration
(including a compatible H1 snippet), then test and start nginx explicitly.
