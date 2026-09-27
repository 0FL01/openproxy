---
name: openproxy
description: Build, initialize, and operate the 0FL01 OpenProxy fork from source or Docker Compose. Use for local router setup, provider and key configuration, and connecting AI clients to OpenProxy.
---

# OpenProxy fork — build & operate

[0FL01/openproxy](https://github.com/0FL01/openproxy) is a local AI proxy router
with a dashboard and OpenAI-compatible endpoint on `127.0.0.1:4623`. It owns
credentials, OAuth, configured routing, and protocol mapping. Clients own
history, compaction, general tool execution, and timed generation retries.

Build from this fork's checkout or Docker Compose. Fork-specific prebuilt
binaries are not guaranteed. Do not download upstream binaries as a substitute
for this fork. There is no shell or PowerShell installer.

## 0 · Inspect before changing state

Check the checkout origin and any existing binary version:

```bash
git remote -v
command -v openproxy
openproxy --version
```

A binary on PATH may be upstream or an older build; its presence alone does
not establish fork provenance. Use the binary built from the fork checkout.
Check whether the intended data directory already contains `openproxy.sqlite`
without dumping database contents or credentials. Preserve existing data,
encryption keys, and deployment environment when rebuilding or upgrading.
Do not stop an existing instance merely to make room for a smoke test; use a
separate `DATA_DIR` and port (the development script defaults to
`~/.openproxy-dev` and `4625`).

## 1 · Build this fork

```bash
git clone https://github.com/0FL01/openproxy.git
cd openproxy
```

Use an existing fork checkout instead when available.

### Docker Compose

```bash
cp .env.example .env.prod
# Privately edit .env.prod before starting: set strong, stable JWT_SECRET
# and OPENPROXY_ENCRYPTION_KEY values.
OPENPROXY_BUILD_COMMIT="$(git rev-parse HEAD)" docker compose up -d --build
curl -fsS http://127.0.0.1:4623/health
```

Only copy the example for a new setup; do not overwrite an existing `.env.prod`.
Compose explicitly loads `.env.prod`, publishes to host loopback, and retains
SQLite state in `openproxy-prod-data`. Preserve that volume on upgrades. Stop
with `docker compose down`; do not add `--volumes` when retaining configuration.
On first startup, retrieve the generated dashboard password privately from
`docker compose logs openproxy`, or set `INITIAL_PASSWORD` before first start.
Do not paste logs containing that password into agent output or issue reports.

### Local source build

Requires Rust **1.85+**, a working C compiler/linker, Node **20.3+**, and pnpm
**10.33.2**. Build the dashboard first because the Rust binary embeds `web/dist`:

```bash
corepack enable
corepack prepare pnpm@10.33.2 --activate
pnpm --dir web install --frozen-lockfile
pnpm --dir web run build
cargo build --release --locked
./target/release/openproxy --version
```

Use `./target/release/openproxy` for the commands below, or put that built binary
on PATH as `openproxy`. Windows source builds produce `openproxy.exe`.
Rebuild the dashboard after changes to `web/src`, then rebuild the Rust binary.

Before storing credentials, privately export strong, stable `JWT_SECRET` and
`OPENPROXY_ENCRYPTION_KEY`. A local binary does **not** load `.env` automatically.
Without `OPENPROXY_ENCRYPTION_KEY`, credentials are stored unencrypted. Retain
the same encryption key for restarts, upgrades, backups, and restores.

## 2 · Initialize and start

For normal setup, start the server and create a client key on the dashboard's
**Endpoint** page:

```bash
openproxy server start --detach --no-open
openproxy --robot server status
curl -fsS http://127.0.0.1:4623/health
```

Default state is `~/.openproxy/openproxy.sqlite`. The first-start password is
printed in server output; keep it private. Use `INITIAL_PASSWORD` only before
first startup to choose it. Follow `openproxy server start --help` to select
an alternate port; use `DATA_DIR` to isolate state.

For autonomous provisioning of an empty data directory, `openproxy --robot
server init` can mint the initial admin key **before** first startup. Its
JSON envelope includes `.data.admin_key.key`. Capture that output directly in
private storage with restrictive permissions (for example, `umask 077` on Unix),
not in a shared `/tmp` file, terminal transcript, or agent response. Do not
run init with `--force` against existing data. For existing deployments, use
the dashboard's normal sign-in and key management instead of resetting a
password to discover it.

Export the selected client key privately as `OPENPROXY_API_KEY`. Do not commit
keys, provider payloads, client config containing tokens, `.env.prod`, or data
directory contents. Prefer a dedicated client key rather than an admin key.

## 3 · Configure providers and models

In the dashboard at `http://127.0.0.1:4623/`, open **Providers**, add a supported
API-key or OAuth connection, then customize **Available Models**. Disabled and
custom models are persisted in SQLite and survive rebuilds.

For CLI provisioning, inspect schemas and command help before creating payloads:

```bash
openproxy schema list
openproxy schema show provider
openproxy schema example provider
openproxy provider apply --help
openproxy key apply --help
```

`provider apply --from-file <private-file>` and `key apply --from-file
<private-file>` support declarative input; `--from-file -` accepts stdin. Keep
credential-bearing payloads private, and avoid `--prune` unless deleting omitted
resources is intended. Provider commands operate on the local database, not
a remote server; use the dashboard for Compose deployments unless deliberately
operating inside the container with its deployment environment and data directory.

OAuth may require user interaction in a browser. Use the supported dashboard
flow rather than assuming every setup step is unattended. Removed provider
backends (including Cursor as a provider, Kiro, Windsurf, Grok Web, and Grok CLI)
are not supported. The retired `combo` schema is compatibility metadata, not
an operational routing command.

## 4 · Connect the client

Get model IDs from the authenticated discovery endpoint:

```bash
curl -fsS http://127.0.0.1:4623/v1/models \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"
```

Configure the URL, client key, and a returned model ID directly in the client's
supported settings. For OpenAI-compatible clients, the base URL is
`http://127.0.0.1:4623/v1`. Check the client's documentation for other protocols
and exact configuration fields. OpenProxy does not read or write client config.

For OpenCode discovery, use the aggregate provider `ludka2` and the separate
[checked-in model plugin](https://github.com/0FL01/openproxy/blob/main/plugins/README.md).
That plugin fetches `/v1/models` at startup; it does not write client JSONC.

## 5 · Verify and operate

After selecting a configured model, send one small request through the client
to verify routing. A model catalog entry alone does not guarantee usable
upstream credentials. Account fallback stays within the chosen provider/model.

```bash
openproxy --robot doctor
openproxy server status
# Stop only the instance you started, when appropriate:
openproxy server stop
```

Fresh installs require API keys and dashboard login by default. Those are
persisted settings; `REQUIRE_API_KEY` is **not** an environment switch. Keep
loopback binding for local use. For a reverse-proxy deployment, consult the
README for `AUTH_COOKIE_SECURE` and `TRUST_PROXY` settings.

| Symptom | Action |
|---|---|
| Port already in use | Inspect the existing service; use an isolated port/data directory for testing. |
| `401` from `/v1/*` | Check the selected client key and server URL privately. |
| Init reports an existing SQLite database | Preserve the data; use normal sign-in/key management. |
| Dashboard blank or stale | Rebuild `web/dist`, rebuild the Rust binary, then reload the browser. |
| OAuth callback fails headlessly | Complete the dashboard flow from a graphical session. |

## References

- [Fork README](https://github.com/0FL01/openproxy#readme)
- [Lean proxy contract](https://github.com/0FL01/openproxy/blob/main/contracts/lean-proxy.md)
- `openproxy --help` and `openproxy <command> --help`
- `openproxy schema list` and `openproxy --robot schema stability`
