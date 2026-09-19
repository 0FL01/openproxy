# OpenProxy architecture

Single-binary AI router: OpenAI-compatible endpoint → provider executors.

The frozen lean ownership and compatibility boundary lives in
[`../contracts/lean-proxy.md`](../contracts/lean-proxy.md). Its route and
protocol-pair inventory is machine-readable in
[`../contracts/lean-proxy.json`](../contracts/lean-proxy.json).

## Request pipeline

1. Detect client format, resolve target format + upstream model.
2. Plan streaming (`forceStream`, Accept header, image-gen).
3. Apply provider thinking, strip lists, tool dedupe.
4. Execute via specialized executor or `DefaultExecutor`.
5. On 401/403 refresh once; after an upstream failure try each remaining eligible account once.
6. Proxy SSE/JSON back, or return the final upstream status and `Retry-After` without a cross-request proxy cooldown.

## Intentional behavior

- SSRF checks on image prefetch.
- Missing credentials fail loud (no `Bearer undefined`).
- Refresh dedup does not cache null failures.
- Every request independently tries active configured accounts; persisted legacy cooldown and health-degrade fields are diagnostic only and never suppress routing.
- Secrets encrypted in SQLite WAL.
- No token-saver passes (PXPIPE/RTK/Headroom/Caveman/Ponytail
  removed) — the proxy forwards bodies unmutated.
- The client harness owns history, compaction, general tool execution, semantic repair,
  and temporal generation retries. The proxy owns credentials/OAuth,
  configured routing, required wire translation, transport, and resource
  limits; it does not become a second harness. Two one-shot exceptions are
  proxy-owned: `/v1/web/fetch` URL extraction and authenticated `/v1/mcp`
  `codex_web_search`, which calls Codex's standalone index rather than a
  Responses generation model. Neither owns history, sessions, or a tool loop.

## Smoke

```bash
./scripts/parity-smoke.sh
cargo test -p openproxy --lib parity_tests stream_flags
```
