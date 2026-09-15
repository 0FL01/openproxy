# OpenProxy architecture

Single-binary AI router: OpenAI-compatible endpoint → provider executors.

## Request pipeline

1. Detect client format, resolve target format + upstream model.
2. Plan streaming (`forceStream`, Accept header, image-gen).
3. Apply provider thinking, strip lists, tool dedupe.
4. Execute via specialized executor or `DefaultExecutor`.
5. On 401/403 refresh once, on 429 try next fallback URL.
6. Proxy SSE/JSON back; selective model-lock clear on success.

## Intentional behavior

- SSRF checks on image prefetch.
- Missing credentials fail loud (no `Bearer undefined`).
- Refresh dedup does not cache null failures.
- Combo quarantine + capacity pre-skip for reliability.
- Secrets encrypted in SQLite WAL.
- No token-saver passes (PXPIPE/RTK/Headroom/Caveman/Ponytail
  removed) — the proxy forwards bodies unmutated.
- Hedging/shadow/auto-combo are scaffold-only until product demand.

## Smoke

```bash
./scripts/parity-smoke.sh
cargo test -p openproxy --lib parity_tests stream_flags
```
