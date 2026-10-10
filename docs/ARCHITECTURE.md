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

## Persistent stream diagnostics

The server always enables **only** the `openproxy::chat::stream` TRACE target
for a dedicated JSONL file sink. `RUST_LOG` / `--log-filter` continue to select
console/dashboard output; even `RUST_LOG=off` does not disable these files.
HTTP/OAuth debug targets, headers, bodies, private account identities and raw
transport error text are not added. Existing bounded, sanitized diagnostics
and UTF-8-bounded provider/model labels are retained, not the SSE frames.

The sink starts after the database has loaded, using the actual `Db::data_dir`
(including permission fallback), under `logs/stream-trace/`. It never writes
TRACE events to SQLite or changes the durability mode of the request journal.
The existing bounded Claude `streamTrace` metadata remains unchanged.

One filesystem thread consumes a nonblocking queue of at most 256 complete
records, each at most 8 KiB (2 MiB queued payload). Full queues, oversized
records and unavailable storage drop diagnostic records, never delay or fail
generation. `logs stats` and authenticated `/api/observability/stats` expose
process-local `traceLogDropped` and `traceLogIoErrors`; counters reset on
restart. Typed warnings are throttled to once per minute and are also emitted
to stderr. They do not log raw errors or recursively enter the file sink.

Storage defaults, including rotation/recovery staging:

- `active.jsonl`: at most 8 MiB, readable while the server runs;
- at most eight numbered `.jsonl.zst` archives, at most 96 MiB combined;
- at most one pending raw segment (8 MiB) and one compressed temporary file
  (16 MiB), for at most **128 MiB of owned file payload**, excluding filesystem
  overhead and copies made by external backup tools;
- Zstd level 3 with checksum, streaming compression outside the request path;
- a 64 MiB free-space reserve: prune eligible old archives first, then suspend
  diagnostic writes when insufficient space remains; local storage recovery
  retries at most once per minute; a failed space probe preserves the archives
  rather than treating unknown capacity as evidence of a full disk;
- retention is size/count based, not a guarantee of a fixed number of days.

Rotation closes the active segment before compressing to a bounded temporary
file. A checked encoder finish and file sync precede atomic, no-replace archive
publication; the raw segment is deleted only afterwards. Startup repairs only
an incomplete active tail and recovers the single pending segment. If
compression fails, it preserves the raw segment; once the active file also
fills, new diagnostics are dropped instead of accumulating raw files.

Linux/macOS storage uses a private directory (0700), private regular files
(0600), no-follow descriptor-relative operations, hardlink rejection and a
nonblocking advisory lock. Do not run multiple instances against one DATA_DIR
or let an external logrotate job manipulate this directory. Unsupported
platforms or unsafe/unavailable paths fail open with diagnostics unavailable.
Zstd is **compression, not encryption**; do not publish or commit these files.

Ordinary `logs tail`, `logs export` and `logs clear` still operate on the
400-line memory buffer, not the disk history. For Compose, read live files with:

```bash
docker compose exec -T openproxy sh -c 'tail -F "$DATA_DIR/logs/stream-trace/active.jsonl"'
```

Read a closed archive with `zstd -dc <archive.jsonl.zst>` on a machine with the
Zstd CLI (the application does not need that executable). The existing data
volume preserves files across container recreation. The application's JSON
backups in `db_backups/` ignore the sibling logs directory; whole-volume
backups include it unless explicitly excluded.

Normal server return attempts a drain with a two-second budget. Signal handling
is unchanged: SIGTERM/SIGKILL may lose queued records and a partial final record,
while already-written prefixes survive a process restart. This is explicitly
best-effort diagnostics, not an audit-grade lossless journal or per-record
power-loss durability. Logger I/O failures never change stream errors, retries,
completion, cancellation, usage or request-log status.

## Smoke

```bash
cargo test -p openproxy --lib parity_tests stream_flags
```
