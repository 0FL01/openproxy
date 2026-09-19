# OpenProxy — Rust AI Proxy Router

## What
OpenProxy is an AI proxy router written in Rust — OpenAI-compatible endpoint that routes requests to 40+ AI providers with format translation, account fallback, token refresh, usage tracking, and SSE streaming.

## Why
Own single-binary AI router: faster, safer Rust implementation. Critical patterns: type-safe format handling, encrypted secrets, immutable data flow, thread-safe by design.

## How (Architecture)
- **Core**: model parsing → format detection → request translation → provider execution → response translation → SSE streaming
- **Account mgmt**: credential selection → token refresh → model-level account fallback
- **Executor trait**: `ProviderExecutor` with default+specialized impls
- **Persistence**: SQLite WAL + encrypted columns + usage tracking
- **Security**: HMAC API keys, bcrypt auth, SSRF protection

## Lean Proxy Boundary

The frozen ownership, route/protocol, migration, and preservation contract is
[`contracts/lean-proxy.md`](contracts/lean-proxy.md), with a machine-readable
manifest in [`contracts/lean-proxy.json`](contracts/lean-proxy.json). The client
harness owns history, compaction, general tool execution, semantic repair, and temporal
generation retries; OpenProxy owns private credentials/OAuth, configured
routing, required protocol mapping, transport reuse, and bounded resources.
Only `/v1/web/fetch` and the stateless authenticated `/v1/mcp` Codex search tool
are proxy-owned one-shot tools; neither may grow history, sessions, or a tool loop.

## Beads
Fork: this is an independent product path; parity with 9router, OmniRoute, or other upstream routers is not a product requirement and is not tracked. Use beads only for own product tasks.

## Key References
- `docs/ARCHITECTURE.md` — pipeline order, intentional behavior, executor dispatch
- `src/server/api/codex_web_mcp.rs` — the single-tool MCP protocol/auth boundary; `src/server/codex_search.rs` owns standalone indexed-search account routing

## Dev Workflow — backend + dashboard rebuild

Single smooth loop — backend and dashboard are **separate builds** served by the same binary:

```bash
./scripts/dev.sh              # incremental cargo build --bin openproxy + run on :4625 (foreground)
./scripts/dev.sh detach       # build + run detached
./scripts/dev.sh build        # only cargo build, don't run
```

The dev server uses port `4625` and `~/.openproxy-dev` by default so it cannot
stop or read the production Compose instance on `4623` and its persistent volume.
Override them with `PORT` and `DATA_DIR` when needed.

**Dashboard is not live-reloaded.** `web/src` → `web/dist` (Astro) is what the Rust server serves.
After any `web/src` change you **must** rebuild the dashboard or the feature will be invisible
(past "feature not found" confusion was a missing rebuild, not a missing backend):

```bash
cd web && pnpm install        # once
pnpm build                    # rebuild web/dist after every web/src change
# or during iteration:
pnpm dev                      # Astro dev on :4624 (proxy API to :4625)
```

Full loop for a feature touching both layers:

```bash
./scripts/dev.sh build && (cd web && pnpm build) && ./scripts/dev.sh detach
curl -s http://127.0.0.1:4625/health
open http://127.0.0.1:4625/dashboard/providers
```

## Contributing & Git Hygiene

Use Conventional Commits and keep each commit a single, independently buildable
change. Stage only intended files. Before committing, inspect `git status`, the
staged diff, and secret handling; run Rust formatting, clippy, and relevant
tests. Run `pnpm --dir web run build` when changing the dashboard.

PRs use [`.github/pull_request_template.md`](.github/pull_request_template.md);
bugs and feature requests use [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/).
CI builds the dashboard, runs Rust formatting/clippy on Ubuntu and macOS, and
runs Rust library tests on Ubuntu. `astro check` is currently advisory in CI.

## Core Product Surfaces (TOP PRIORITY)

These 3 surfaces ARE the product. Everything else is optional. They must be flawless, reliable, and mutually consistent — always prioritize regressions and improvements here:

1. **Providers page** — `/dashboard/providers/<provider>` (e.g. kilocode): user controls Available Models (disable/enable/custom). Configuration is user data, persisted in SQLite — must survive binary rebuilds/updates.
2. **CLI tools config** — `/dashboard/cli-tools/opencode` (opencode is the primary client).
3. **`web/src/shared/components/ModelSelectModal.tsx`** — the single model-picker used everywhere; must exactly mirror the provider page's Available Models (same disabled map + custom rows + catalog merge). Any change to model-list logic MUST be applied consistently to both the provider page and this modal.

Core workflow that must never break: configure provider → customize available models → select models for opencode CLI config.

## OpenCode Model Discovery
- `plugins/openproxy-models.js` is the supported fetch path for the aggregate `ludka2` provider; OpenCode auto-loads its installed copy from `~/.config/opencode/plugins/` and fetches `/v1/models` at startup.
- `/v1/models` supplies canonical `opencode.source`; the plugin appends it to every display name. Never infer the upstream provider from configurable route prefixes. See `plugins/README.md` and run the two `tests/opencode_models*.test.mjs` checks after changes.

## Status
Active fork. Run `cargo test -p openproxy --lib parity_tests stream_flags` for smoke.

## Local Config & Secrets — Never Commit
- **Do not commit** local user config or secrets: `opencode.json`, `.env`, `.env.*`, `*.pem`, `~/.openproxy/db.json`, `~/.openproxy/admin.key`, API keys, `provider_specific_data` with live credentials, or any file containing `sk-`, `Bearer`, `refresh_token`.
- `opencode.json` is local agent config (model, MCP keys like `CONTEXT7_API_KEY`, permissions) — keep untracked. `scripts/dev.sh` builds locally; real secrets live in SQLite (encrypted provider fields) + `OPENPROXY_API_KEY` env, not in git.
- Before `git add`/`commit`, run `git status` and `git diff --cached`; if a file contains secrets or is machine-local, `git restore --staged <file>` and add it to `.gitignore`. Prefer `git check-ignore -v <file>` to verify.
- If a secret is accidentally committed, rotate it immediately and purge history (`git filter-repo` or BFG) — do not just revert.

## Schema stability (`openproxy.v1.*`)

The `openproxy.v1.*` envelope namespace is a **frozen, additive-only contract**. Every JSON envelope emitted by `--robot` carries a `schema` field matching `openproxy.v1.<area>.<action>`. Existing fields keep their names, types, and meanings across releases. New fields are additive only — no renames or removals. A new `openproxy.v2.*` namespace will be opened before any breaking change.

Run `openproxy schema stability` to see the current stability promise:

```bash
openproxy --robot schema stability
# → {"schema":"openproxy.v1.schema.stability","data":{"namespace":"openproxy.v1","stability":"stable","policy":"..."}}
```

The `schema` subcommand provides four operations:

| Command | Purpose |
|---|---|
| `openproxy schema list` | List all resource kinds with schema and example support |
| `openproxy schema show <resource>` | Print JSON Schema for a resource (provider, key, etc.) |
| `openproxy schema example <resource>` | Print an example payload for a resource |
| `openproxy schema stability` | Print the v1 namespace stability contract |

13 resources are covered. The retired `combo` resource remains schema/example-only compatibility metadata under the frozen v1 contract; it has no operational command or route.
