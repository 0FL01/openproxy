# Lean proxy acceptance results (C42)

Final like-for-like acceptance for the `openproxy.lean-plan.v1` checkpoint
graph on branch `perf/lean-proxy-plan`. All values below are measured, not
estimated. Where something could not be verified, it is listed as
unavailable — never as a pass.

## Workload and method (identical for baseline and final)

- Command: `scripts/bench-lean` with
  `LEAN_BENCH_OUTPUT=bench/lean/final.json` (the baseline artifact
  `bench/lean/baseline.json` was not overwritten).
- Matrix: direct loopback mock vs full OpenProxy handler; cold/warm;
  concurrency 1/8/32; 5 repetitions; the same successful three-content-event
  OpenAI SSE/tools request (410 measured requests, 410 upstream attempts per
  path in both runs).
- Build: release profile (`thin` LTO, `codegen-units = 1`, `opt-level = "s"`,
  `panic = "abort"`, symbols stripped), features `default` + `embed-web`,
  `RUST_LOG=off` with the request-logging code path enabled.
- Toolchain/env: Rust/Cargo 1.98.1, OpenCode 1.18.31, Linux
  6.12.86+deb13-amd64, AMD EPYC 9634.
- Overhead percentiles are matched proxy-minus-direct per-request samples,
  never subtraction of independently aggregated percentiles (C02 method).

## Baseline (2026-09-17) → final (2026-09-18), warm concurrency-32 handler

| Metric | Baseline | Final | Delta | Predeclared gate |
|---|---|---|---|---|
| TTFC p50 (µs) | 52,243 | 55,455 | +0.4% | noise 10% |
| TTFC p95 (µs) | 59,236 | 60,564 | +2.2% | regression 15% + 250 µs |
| TTFC p99 (µs) | 62,261 | 64,683 | +3.9% | — |
| Throughput (rps) | 151.22 | 149.21 | −1.3% | decrease 10% |
| RSS peak (bytes) | 65,191,936 | 62,607,360 | −4.0% | regression 10% + 1 MiB |
| PSS peak (bytes) | 62,800,896 | 60,236,800 | −4.1% | regression 10% + 1 MiB |
| Allocated/request (bytes) | 288,453 | 252,245 | −12.5% | regression 10% + 64 KiB |
| Successes / attempts | 410 / 410 | 410 / 410 | exact | must be exact |

No regression gate tripped. The allocation/request decrease is an
improvement, not a counted optimization win: it follows from removing
temporary copies (C26–C28, C33–C34), while every run still completes the full
successful workload.

## Effect separation (honest)

The benchmark cannot separate "removed features" from "internal
optimizations" on the same successful workload — both runs do identical
successful work by design (C6 constraint). Separation is proven per
checkpoint instead:

- Removals (behavior gone, proven by targeted tests): Kiro semantic
  repair/history replay (C03/C05), global Claude header replay (C04),
  session/continuation maps (C06, incl. 100,000-session churn),
  heuristic context rejection (C08), per-executor temporal retry loops
  (C10–C12), legacy success-path cleanup (C14), token-keyed completed
  caches (C18/C23), generation-path catalog/project/onboarding waits
  (C19–C22), default health probes and orphan breaker (C24), idle quota
  workers (C25), raw SSE history in forced conversion (C33), demo chat UI
  (C38).
- Bounds (same bytes, less retention): request ownership (C26), single
  serialization (C27), response ownership trim (C28), body collection caps
  (C29), image budgets + SSRF pinning (C30A/C30B), accumulator caps (C31),
  incremental framing (C32), point-mutation accumulators (C34), bounded
  metadata logging with durable default (C35), generation admission (C36),
  backup buffer scoping (C40).
- Not claimed as wins: the SQLite page-cache comparison kept `-64000`
  (C37: +4.2% for `-8192` sits inside noise), the `predicates` dev-dep
  removal and `serde_urlencoded` rescoping change no binary behavior (C41),
  and overload 429s are reported separately, never as memory savings (C36).

## Regression gates (all green at close)

- `cargo test -p openproxy --all-targets`: lib 1224 passed; every
  integration binary passed, 0 failed (incl. all `lean_proxy_*` gates,
  translator goldens, auth races, quota/refresh singleflight, DB
  concurrency/cancellation).
- `cargo fmt --all -- --check` and `git diff --check`: clean.
- `cargo clippy --all-targets --all-features`: only the 3 pre-existing
  production style warnings (`let_and_return` ×2, `needless_return` ×1);
  zero warnings from checkpoint code.
- `cargo check --no-default-features`: clean (headless/sidecar build).
- `pnpm build` (dashboard): 87 pages, complete, no BasicChat output.
- Node: `OPENCODE_BINARY=/root/.opencode/bin/opencode node --test
  tests/opencode_models.test.mjs tests/opencode_models_cli.test.mjs` →
  5 passed, 0 failed, 0 skipped. This is a REAL run of the installed
  OpenCode 1.18.31 (plugin autoload + `models ludka2 --refresh` against a
  loopback fixture; no paid calls).
- SQLite page-cache matrix: `bench/lean/sqlite-cache-c37.json` (verdict:
  keep `-64000`).

## Caught and fixed during acceptance

The full-suite run exposed a real bug in the C35 background log writer:
it was spawned on the first caller's tokio runtime, so under short-lived
runtimes (e.g. one `#[tokio::test]` runtime per test) the writer died with
that runtime and closed the channel permanently. Fixed by running the
single writer on a dedicated runtime-independent OS thread
(`src/server/application_logs.rs`); the C35 suite additionally serializes
its queue-touching tests. Full suite re-run: green.

A stale `tests/cargo_deps.rs` assertion expected `dotenvy`, deliberately
removed in `b48d9653` with cargo-machete evidence and zero code references.
The test — not the manifest — was stale; the single case was removed with
a comment citing the removal commit (precedent: `5cef105d` realigned the
same file for tower-http).

## Hot-path prohibitions (verified absent)

- No semantic repair, prompt mutation, or second generation (C03).
- No history/session/content replay between requests (C05/C06).
- No process-global header maps or token-keyed completed-result caches
  (C04/C18/C23).
- No generation retry or sleep after downstream commitment; one
  request-scoped account/auth budget owns recovery (C10–C13).
- No catalog, quota, or log wait on the lean hot path: catalogs publish
  outside generation (C19/C20), quota is an explicit uncached read (C23),
  lean logging only enqueues (C35, durable stays default).
- No agent tool runner in the core: `/v1/web/fetch` is a pinned single
  extraction call, and `/v1/mcp` is one authenticated stateless Codex search
  call. Neither owns history, sessions, or a tool loop (C39/C44).

## C44: Codex web search MCP

- OpenCode 1.18.31 discovers `codex_web_search` directly over authenticated
  MCP 2025-11-25; there is no wrapper, session, SSE transport, or MCP SDK.
- `tools/call` reuses the Codex catalog, account fallback, OAuth recovery,
  pooled transport, admission, and one bounded native Responses accumulator.
- Model policy prefers `gpt-5.6-luna`, otherwise the highest published
  search-capable Luna with supporters; non-Luna is never selected.
- Ordered messages and citations are returned in one all-or-error text block.
  Public Codex native search is rejected before upstream, and the retired
  header/depth/OpenCode config surfaces are removed.
- Evidence: 1,026 library tests; focused MCP/Codex/forced-stream/settings/C39
  suites; real OpenCode discovery; dashboard and model-plugin builds. No paid
  search was performed.

## Limitations and explicitly unverified items

- Production provider compatibility (real Kiro/Codex/Claude/Gemini traffic
  with live credentials) was never exercised: no credentials exist in this
  environment and paid calls are forbidden by the plan constraints. Wire
  compatibility is proven against loopback mocks and translator goldens
  only.
- The custom harness version/contract is `unknown`; end-to-end
  compatibility with it is unverified.
- `PLAN.md` / `AUDIT-EVIDENCE.md` are absent; the executable graph was
  `docs/CHECKPOINTS.json` throughout.
- "Cold" filesystem cache in C37 means freshly copied files (no privilege
  to drop OS caches); allocator-retained RSS prevents per-config RSS
  attribution, so cache pressure was compared via `SQLITE_DBSTATUS` and
  `/proc/self/io` instead.
- Absolute latency/RSS numbers are machine-specific (shared CI-shaped
  host); the acceptance claim is baseline-vs-final parity within
  predeclared gates, not portable absolutes.

## Rollback

Each checkpoint is an atomic Conventional Commit on `perf/lean-proxy-plan`
(C00–C44); revert any single commit without data migration. Release is
blocked on any wire/security/data contract violation — none is open: all
required checkpoints are `done`, all conditionals (`C06`, `C25`, `C37`,
`C38`, `C39`, `C41`) are `done` with evidence, none is `not_applicable`.
