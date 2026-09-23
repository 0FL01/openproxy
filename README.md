# OpenProxy

<div align="center">
  <img src="openproxy_illustration.webp" alt="OpenProxy — local AI proxy router">
</div>

**A local AI proxy router for existing clients.** This is the
[0FL01 fork](https://github.com/0FL01/openproxy) of
[quangdang46/openproxy](https://github.com/quangdang46/openproxy). It keeps
provider credentials and OAuth in the proxy, routes configured models to
eligible accounts, translates the required API formats, and serves a dashboard
from the same Rust binary. It is not an agent runtime: the client owns history,
compaction, general tool execution, semantic repair, and timed generation
retries. See the [lean proxy contract](contracts/lean-proxy.md) for the exact
boundary.

## Start this fork

There are **no fork-specific prebuilt releases** yet. The checked-in
`install.sh` and `install.ps1` download **upstream** `quangdang46/openproxy`
releases, not this fork. To run the fork, build from this checkout or use Docker
Compose.

### Docker Compose

```bash
git clone https://github.com/0FL01/openproxy.git
cd openproxy
cp .env.example .env.prod
# Edit .env.prod: replace JWT_SECRET and set a strong, stable
# OPENPROXY_ENCRYPTION_KEY (see Configuration below).
docker compose up -d --build
curl -fsS http://127.0.0.1:4623/health
```

Open `http://127.0.0.1:4623/`. For a fresh database, find the generated
dashboard password in `docker compose logs openproxy` (or set
`INITIAL_PASSWORD` before the first start). Keep that output private. Compose
binds the host port to loopback and keeps the SQLite database and credentials
in the `openproxy-prod-data` volume. `docker compose down` stops the service;
do not use `--volumes` if you want to retain its configuration.

### Build from source

Requires Rust **1.85+**, a working C compiler/linker, Node **20.3+**, and
pnpm **10.33.2**. From the checkout:

```bash
corepack enable
corepack prepare pnpm@10.33.2 --activate
pnpm --dir web install --frozen-lockfile
pnpm --dir web run build
cargo build --release --locked
./target/release/openproxy --no-open
```

The dashboard is embedded at build time, so build `web/dist` before the Rust
binary. By default the server listens on `127.0.0.1:4623` and stores state
under `~/.openproxy/` (`openproxy.sqlite`). The first-start dashboard password
is printed in the server output. For a non-Compose deployment, export stable
`JWT_SECRET` and `OPENPROXY_ENCRYPTION_KEY` values before adding credentials;
the binary does not load `.env` automatically. For an isolated development
server on port `4625` with separate data under `~/.openproxy-dev`, use
`./scripts/dev.sh` after building the dashboard. Rebuild the dashboard after
changes to `web/src`.

## Configure a provider and client

1. Sign in to the dashboard at `http://127.0.0.1:4623/`. Its **Endpoint** page
   manages API keys; create or select a key for your client.
2. Go to **Providers**, choose a provider, and add an account/connection with
   its supported OAuth or API-key method. Custom compatible endpoints are also
   available.
3. On that provider's **Available Models** section, disable models you do not
   want offered, restore them from **Disabled models**, or add a custom model
   ID. These choices are stored in SQLite, not in the built dashboard assets.
4. Configure your client directly: use `http://127.0.0.1:4623/v1` as its
   OpenAI-compatible base URL, an OpenProxy API key, and a model ID from
   `/v1/models`. OpenProxy does not read or write client configuration files.

For automatic OpenCode model discovery, configure the aggregate provider as
`ludka2` and install [the separate OpenCode plugin](plugins/README.md). The
plugin fetches `/v1/models`; it is independent of client configuration and
does not write JSONC to disk.

Other clients that accept an OpenAI-compatible endpoint can use
`http://127.0.0.1:4623/v1` as their base URL and an OpenProxy API key. Select a
model ID from `/v1/models` after configuring a provider; a catalog entry alone
does not guarantee a working upstream connection.

## Routes and routing behavior

| Interface | Route |
|---|---|
| OpenAI Chat Completions | `POST /v1/chat/completions` |
| Anthropic Messages and count tokens | `POST /v1/messages`, `POST /v1/messages/count_tokens` |
| OpenAI Responses | `POST /v1/responses` |
| Gemini-style models/API | `GET /v1beta/models`, `POST /v1beta/models/{...}` |
| Ollama-style chat | `POST /v1/api/chat` |
| Model discovery | `GET /v1/models` |
| One-shot URL extraction | `POST /v1/web/fetch` |
| One-shot Codex indexed web search (MCP) | `POST /v1/mcp` |
| Liveness (no auth) | `GET /health` |

Fresh installs require an API key for chat and model discovery. Export a key
from the dashboard as `OPENPROXY_API_KEY` in your shell. For example, with a
configured Claude Code connection:

```bash
curl -fsS http://127.0.0.1:4623/v1/models \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"
curl -fsS http://127.0.0.1:4623/v1/chat/completions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"cc/claude-opus-4-7","messages":[{"role":"user","content":"Hi"}]}'
```

Routing uses the selected provider/model. If an upstream attempt fails, the
proxy can try another **eligible account for that same provider/model** within
the request; it does not switch to an unrelated provider or schedule timed
generation retries. Invalid-request upstream responses (400/413/422) are
terminal. A final upstream failure preserves its status, body, and
`Retry-After` (apart from a documented 429 reset-message suffix). Streaming
responses are not retried after downstream commitment.

The proxy does not execute general client tools. `/v1/web/fetch` is a single
configured-provider URL extraction request. `/v1/mcp` is a stateless,
authenticated POST/JSON MCP server with one wire-level tool, `search` (shown
as `codex_web_search` by OpenCode). It needs a configured Codex account and
uses Codex's standalone indexed-search endpoint, **not** a Responses
generation call. It accepts `query` and optional `response_length` (`short`,
`medium`, `long`). The server supports MCP protocol version `2025-11-25`;
it has no MCP sessions or SSE transport. Native Codex `web_search` in
generation requests is rejected; configure the MCP endpoint instead:

```json
{
  "mcp": {
    "codex_web": {
      "type": "remote",
      "url": "http://127.0.0.1:4623/v1/mcp",
      "enabled": true,
      "oauth": false,
      "headers": { "Authorization": "Bearer <openproxy-api-key>" },
      "timeout": 30000
    }
  }
}
```

Removed backends (Kiro, Cursor as a **provider**, Windsurf, Grok Web and Grok
CLI) are not supported by this fork. This does not prevent using a client
such as Cursor against a compatible proxy endpoint, or using the separate
xAI API-key provider. Legacy saved configuration is retained for round-trip
compatibility, not reactivated.

## Configuration and CLI

| Setting | Default / effect |
|---|---|
| `HOSTNAME`, `PORT` | `127.0.0.1`, `4623` for local runs; Compose listens inside the container on `0.0.0.0` but publishes only to host loopback. |
| `DATA_DIR` | `~/.openproxy` on typical Unix installs; contains `openproxy.sqlite`, keys, and backups. Keep it backed up and private. |
| `JWT_SECRET` | Random per process when unset. Set a stable, strong value so dashboard sessions survive restarts. |
| `OPENPROXY_ENCRYPTION_KEY` | Unset means **credentials are stored unencrypted**. Set a stable strong key before adding credentials, and retain it for restores. |
| `INITIAL_PASSWORD` | If unset on a fresh install, a random password is generated, stored under the data directory, and printed at first startup. |
| `API_KEY_SECRET` | Random and persisted under the data directory when unset; protects generated API keys. |
| `AUTH_COOKIE_SECURE` | Set `true` when serving the dashboard over HTTPS. |
| `TRUST_PROXY` | Set `true` only behind a trusted reverse proxy that controls forwarded headers. |
| `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` | Optional outbound provider proxy settings. |

The persisted settings `requireApiKey` and `requireLogin` default to **true**
on fresh installs. `REQUIRE_API_KEY` is **not** an environment switch. Use
dashboard settings for policy changes; do not expose an unauthenticated
instance to the network. Compose loads `.env.prod` explicitly; a local binary
does **not** automatically read `.env` files, so export its environment before
starting it.

```bash
openproxy --help
openproxy provider list            # configured connections in the local DB
openproxy models list              # local model catalog
openproxy --robot schema stability # additive-only openproxy.v1.* envelopes
openproxy --robot doctor
openproxy server status
```

Local provider/key/model commands work against the data directory. The
`settings`, `chat`, and `logs` CLI commands can talk to a running server via
`--url` and `--api-key` (or `OPENPROXY_API_KEY`); `provider apply` is a local
database operation, not a remote API call. `openproxy --robot server init` can
mint an initial admin key **before** first startup on an empty data directory;
store that secret privately and do not use `--force` on existing data. See
`openproxy <command> --help` and the
[agent setup skill](.agents/skills/openproxy/SKILL.md) for more CLI details
(note that its installer path currently targets upstream).

## Development and provenance

Rust 2024, axum, SQLite WAL, and an embedded Astro 5 / React 19 dashboard.
For development details see [AGENTS.md](AGENTS.md); for supported routes and
product boundaries see [contracts/lean-proxy.md](contracts/lean-proxy.md).
This fork is an independent product path, not a promise of upstream feature
parity. Original project: [quangdang46/openproxy](https://github.com/quangdang46/openproxy).

MIT — see [LICENSE](LICENSE).
