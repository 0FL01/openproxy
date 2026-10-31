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

## Native Claude Agent exception

`claude-agent` replaces the retired `claude`/`cc` subscription OAuth/C46
impersonation path; Anthropic API-key and shared Messages transports are unchanged.
Only this provider bypasses the ordinary HTTP executor pipeline. The authenticated
OpenCode V2 adapter sends LanguageModelV3 input to `/v2/claude-agent/stream`;
`/idle` forwards terminal client-execution events. Authentication precedes JSON
parsing, generation reuses the 32 MiB limit and lifetime admission permit, and
session/fork IDs are HMAC-scoped to the authenticated key before entering a child.

`server/claude_agent.rs` lazily supervises one Node bridge on bounded JSONL stdio.
`runtime/claude-agent` uses the official SDK with an explicit original installed
Claude executable. Native Claude owns credentials, HTTP, retry policy and saved
transcripts. OpenCode owns authoritative history, tools/permissions and compaction;
the proxy only transports MCP tool calls/results, never executes client tools.
Native-session checkpoints contain hashes/UUIDs, not another message database.
Persistent primary sessions resume/fork; auxiliary calls use disposable branches.
Eight processes are allowed; tool waits do not arm five-minute idle. Cancellation
interrupts/drains native work before releasing parked callbacks; shutdown reaps
the owned process group. There is no second generation retry or API-key fallback.

The dashboard relays one fixed `claude auth login` PTY, with same-origin/identity
checks, a ten-minute deadline and at most 64 KiB of terminal output. Only native
status JSON and a prompt-free SDK probe can complete login. The app DB stores an
external marker and safe inventory/account metadata, never native tokens.
`$DATA_DIR/claude-agent` isolates native HOME/config from host and API/cloud
overrides and persists in the existing volume. Native credentials/transcripts
are not included in SQLite JSON backups; volume backups must be protected.
Child stderr, SDK debug capture and login URL/code/transcripts are not logged.
Catalog reads use saved inventory without starting Claude; native process starts
refresh that inventory while preserving previously probed context windows.

## xAI model discovery

xAI chat/vision inventory comes from authenticated `/v1/language-models`;
`/v1/models` optionally adds context lengths, never membership. Official aliases
are advertised as unchanged upstream model IDs. Multi-agent families and their
aliases are excluded from automatic discovery because the current xAI transport
is Chat Completions. Explicit user models remain configuration-owned.

`server/xai_catalog.rs` maintains a credential-scoped, process-local cache shared
by dashboard and client discovery: one-hour success freshness, 60-second failed
automatic retry, and singleflight manual/automatic refresh. No worker, disk cache,
configuration import or generation probe is created. Refresh uses the configured
proxy, a pooled redirect-disabled client, a ten-second caller deadline, 1 MiB
response limits and at most 2,048 expanded identities. Missing metadata remains
unknown; optional context-enrichment failure does not discard valid model IDs.
These transport limits apply to the new catalog GETs. OAuth 401 recovery joins
the existing refresh coordinator; its legacy transport is unchanged, and an
already-issued rotated token pair is still persisted if the catalog waiter ends.

Same-identity last-good data survives refresh failure. Cold public discovery
fails explicitly instead of silently omitting configured automatic xAI models;
admin aggregates retain other providers and expose xAI diagnostics. Explicit
enabled-model overrides remain usable without discovery. Generation and health
never fetch this catalog. User overlays and canonical `opencode.source = "xai"`
are preserved. The local CLI's static model list is not a server discovery view.

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

`stream_start.attempt_id` links a stream to the existing Application Logs
`requestId` (a server-generated UUID, not a client session or provider ID).
An early generator drop emits `stream_end` with `reason: "body_drop"`, the
observed `native_response_completed` state and `upstream_eof`; it does not
claim a user Stop or physical disconnect. `attempt_drop` records the same
`attempt_id`, `finish_status` (`not_started` or the in-progress finish status),
logging mode, selected drop `status`, and `finish_enqueued`. The retained
`interrupted_enqueued` is true only for an enqueued interrupted update. Enqueue
success is not proof of a SQLite write. These diagnostics do not change
passthrough bytes, cancellation, or the public request-log schema.

Native Responses attempt accounting recognizes a validated successful
`response.completed` before downstream yield, independently of TRACE. If the
body is then dropped before EOF, the existing bounded drop pipeline preserves
`success`, final token counters and original TPS evidence instead of inventing
an interruption. Early drops remain `interrupted`; observed protocol failures
or framing errors revoke that fallback, and explicit error finishes still win.
Reading continues normally after completion, so later observed errors are not
suppressed. Duration still ends at finish/drop. Success describes the observed
generation result, not guaranteed downstream delivery. Drop writes remain
best-effort in both logging modes; old rows and startup recovery are unchanged.

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
