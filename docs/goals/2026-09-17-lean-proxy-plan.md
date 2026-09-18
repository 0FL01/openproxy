# Goal: Complete the lean proxy plan

Status: active
Source: [user instruction](../AGENT-PROMPT.md), [checkpoint tracker](../CHECKPOINTS.json), and the user request of 2026-09-17 to execute every checkpoint iteratively on a new branch with commits after major changes
Last updated: 2026-09-18

## Objective

Deliver the complete `openproxy.lean-plan.v1` checkpoint graph: OpenProxy remains a bounded, low-overhead provider/auth/protocol router while the client harness owns history, compaction, tools, semantic repair, and temporal generation retries.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved checkpoint in dependency order. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Commit each independently buildable major checkpoint before starting the next. Finish when every required checkpoint is `done`, every conditional checkpoint is either `done` or evidence-backed `not_applicable`, the final measurements are recorded, and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes

- R1: Freeze and verify the lean-proxy ownership, compatibility, and migration contract (C00).
  - Source: `docs/CHECKPOINTS.json` C00.
  - Acceptance: `contracts/lean-proxy.md` and its machine-readable manifest identify owners, supported routes/protocol mappings, preserved product chains, removed automatic behavior, version evidence, and non-destructive legacy-setting rules; `AGENTS.md` and `docs/ARCHITECTURE.md` link to the contract.
  - Primary evidence: JSON validation plus focused schema/model-discovery tests listed by C00.
  - Status: verified
  - Evidence: `contracts/lean-proxy.{md,json}` freeze the ownership, route/format, preservation, and non-destructive legacy-data contract. Four manifest tests, 11 translator parity tests, seven `/v1/models` tests, OpenCode discovery tests (4 pass/1 installed-CLI skip), clippy, and all 1,233 library tests passed.

- R2: Establish a deterministic mock-upstream harness and comparable baseline (C01-C02).
  - Source: `docs/CHECKPOINTS.json` C01-C02.
  - Acceptance: The harness controls headers, chunk timing/EOF, failures, OAuth rotation, request counts, clock, and concurrency; release baseline records the specified latency, memory, allocation, throughput, feature, workload, and environment metadata with at least five representative repetitions.
  - Primary evidence: Harness tests and validated `baseline.json` produced by the documented benchmark command.
  - Status: verified
  - Evidence: C01 is verified: the reusable loopback harness controls response headers/status/chunks, first-byte and EOF gates, body failures, OAuth-shaped rotation, and request capture/counts; native JSON/SSE and OpenAI↔Claude fixtures, virtual time, barriers, and an owned temporary DB are covered by five passing integration tests. C02 is verified by `scripts/bench-lean`: its release latency/RSS/PSS run and separate counting-allocator run produced `bench/lean/baseline.json`, covering direct/full-handler, cold/warm, concurrency 1/8/32, five repetitions, matched per-request overhead, exact success/attempt counts, and predeclared thresholds. Two artifact validation tests passed.

- R3: Remove proxy-owned semantic replay, header replay, context policy, and independent generation retry loops while preserving required protocol/account behavior (C03-C14).
  - Source: `docs/CHECKPOINTS.json` C03-C14.
  - Acceptance: Every checkpoint acceptance statement C03-C14 is met in dependency order, including an evidence-backed disposition for conditional C06 and a single documented attempt budget.
  - Primary evidence: Each checkpoint's targeted mock tests and affected routing/translator regression gates.
  - Status: verified
  - Evidence: C03-C13 and C14 are complete. Semantic repair/history and header replay are deleted; session identifiers are stateless; context metadata is separated from client-owned policy; heuristic rejection is gone; passthrough is protocol-keyed; Default, Codex, and Antigravity temporal loops are gone; and C13 proves one request-scoped account/auth generation budget with terminal client errors, one counted auth recovery, raw error preservation, cancellation, and no post-commit retry.

- R4: Bound persistence, OAuth coordination, catalogs, control-plane state, and background work by configured connections rather than request/token history (C15-C25).
  - Source: `docs/CHECKPOINTS.json` C15-C25.
  - Acceptance: Every checkpoint acceptance statement C15-C25 is met, including foreground/control-plane refresh migration before old cache removal and an evidence-backed disposition for C25.
  - Primary evidence: DB concurrency/cancellation tests, OAuth singleflight tests, catalog/project/onboarding/quota/health mock tests, and background-task lifecycle tests.
  - Status: verified
  - Evidence: C15-C25 are complete. Database publication is cancellation-safe; every configured OAuth refresh caller uses active-only connection/generation coordination; completed token/quota caches are gone; model catalogs publish outside generation; Antigravity project/onboarding state is connection-scoped; default health probing and orphan breaker state are gone; quota auto-ping has no idle worker without an explicit eligible opt-in.

- R5: Reduce payload copies and bound all collected response, image, tool, and SSE state without changing valid wire semantics (C26-C34).
  - Source: `docs/CHECKPOINTS.json` C26-C34.
  - Acceptance: Every checkpoint acceptance statement C26-C34 is met; native streams remain streaming and valid golden payloads remain equivalent.
  - Primary evidence: Ownership/serialization instrumentation, byte-bound edge tests, all-chunk-boundary SSE tests, translator goldens, and allocation measurements.
  - Status: pending
- Evidence: C26-C33 are verified; C34 remains pending, so R5 is not verified.

- R6: Complete bounded logging/admission/runtime lifecycle and resolve optional product-surface checkpoints without harming the three core product surfaces (C35-C41).
  - Source: `docs/CHECKPOINTS.json` C35-C41.
  - Acceptance: Every required checkpoint is done and C37-C39/C41 are done or evidence-backed `not_applicable`; providers, Available Models, `ModelSelectModal`, and OpenCode discovery/config remain mutually consistent.
  - Primary evidence: Logging/admission/backup tests, measured SQLite experiment, dashboard/model-discovery tests, supported-feature builds, and dependency census.
  - Status: pending
  - Evidence:

- R7: Publish final like-for-like acceptance evidence and close the tracker (C42).
  - Source: `docs/CHECKPOINTS.json` C42.
  - Acceptance: The same successful workload and logging contract are measured for baseline/final; all checkpoint states and dependencies are valid; `docs/lean-proxy-results.md` reports real values, limitations, rollback, and unavailable external-provider/OpenCode-harness checks without invented passes.
  - Primary evidence: C42 commands, final artifacts, full affected regression suite, clean diff/secret scan, and checkpoint-graph validation.
  - Status: pending
  - Evidence:

### Constraints

- C1: Never truncate or rewrite client prompt, tools, or supplied history except required wire-format mapping.
- C2: Preserve credentials/OAuth, bounded account fallback, HTTP connection reuse, security state, TLS/SSRF/encryption/auth/audit contracts, and required provider continuation protocol.
- C3: Preserve providers → Available Models → `ModelSelectModal` → OpenCode configuration/discovery, canonical `opencode.source`, and user custom/enabled/disabled models.
- C4: `openproxy.v1.*` remains additive-only; legacy data is preserved and behavior changes are explicitly deprecated/migrated rather than silently reinterpreting stored fields.
- C5: No generation retry or sleep after downstream response commitment; one request-scoped account/auth attempt budget owns allowed recovery.
- C6: Performance comparisons use equal release features, successful workloads, concurrency, and logging semantics; overload rejection is not counted as a memory optimization.
- C7: Tests use mock upstreams and temporary data/config only; no production credentials, production DB, or paid model calls.
- C8: No unrelated dependency/lockfile updates, allocator/runtime-thread changes, whole-file `chat.rs` rewrite, or replacement general-purpose cache framework.

### Non-goals

- Implementing a second agent/harness, tool runner, conversation store, semantic repair layer, compactor, or temporal retry scheduler in the proxy.
- Removing required protocol translation, provider continuation state without a proven stateless equivalent, count-tokens API, provider-native tool/web-search forwarding, or authenticated model discovery.
- Treating reduced successes, disabled logging/security, lower concurrency, or an unverified external provider as performance success.
- Destructive cleanup of legacy user data that can remain inert and round-trip safely.

## Change Envelope

- Target: The C00-C42 scopes and direct consumers declared in `docs/CHECKPOINTS.json`, in dependency order.
- Expected paths, symbols, and direct consumers: `contracts/lean-proxy.*`, lean test/benchmark artifacts, referenced executor/translator/chat/DB/OAuth/catalog/logging modules, directly coupled API/CLI/dashboard consumers, and focused tests/docs.
- Allowed artifacts: Focused production changes, characterization/regression tests, deterministic fixtures, benchmark scripts/results, non-destructive migrations/deprecations, deletion of proven dead code, and dashboard rebuild artifacts when repository convention tracks them.
- Forbidden artifacts: New generic cache/retry frameworks, paid/live-provider test traffic, production data or secrets, unrelated refactors/dependency upgrades, security bypasses, or weakened tests.
- User or harness budget: No explicit LOC/time budget. Use atomic Conventional Commits after each major verified change; work on branch `perf/lean-proxy-plan`.

## Current Checkpoint

- Closes: R6 / C41.
- Smallest next action: Remove only confirmed-dead modules/dependencies left by earlier checkpoints, one group per commit, keeping all supported build features green.
- Expected evidence: full feature-matrix builds, clippy/tests and web build green; lockfile changes only from justified removals.
- Stop or replan if: A needed adapter, pool, security, or crypto path would go dark; restore it instead.

## Current State

- Resolved: R1 / C00, R2 / C01-C02, R3 / C03-C14, R4 / C15-C25, and C26-C34 inside R5. Proxy-owned semantic/header/history replay, heuristic context policy, client-identity passthrough gating, all three executor temporal retry schedulers, successful-response legacy housekeeping, token-keyed completed refresh/quota results, generation-path remote catalog waits, both process-wide Antigravity project maps, request-scoped onboarding workers, default provider health probes, and the write-only circuit breaker are gone. C13 provides the single bounded request-scoped generation/account/auth planner; C16-C18 provide one active-only connection/generation refresh service used by every configured caller; C19-C20 publish immutable OpenCode and Codex model metadata outside generation; C21-C22 make project metadata and onboarding configured-connection lifecycle concerns; C23 makes Claude quota an uncached explicit control-plane read; C24 keeps liveness local and health diagnostics explicit/opt-in; C25 creates no quota auto-ping task without a saved matching connection opt-in and preserves proactive OAuth refresh separately. C26 moves parsed request JSON across the handler's sole-consumer boundary, C27 uses one bounded DefaultExecutor serialization for byte-identical account attempts, C28 removes transformed request JSON from executor response ownership, C29 bounds successful/protocol and diagnostic upstream body collection independently without collecting native chat SSE, C30A bounds only required image expansion with request-scoped decoded/encoded/final budgets and explicit pre-generation failure, C30B binds validated image addresses to the actual socket while preserving hostname/TLS verification and per-redirect validation, C31 bounds wire indices plus retained tool/text/reasoning state, C32 gives live text streams one bounded incremental framing contract without changing native bytes, C33 feeds completed frames incrementally into the forced accumulator without retaining raw SSE history, and C34 keeps Chat-to-Responses accumulation byte-equivalent with point mutation/push_str and frees completed buffers SSE history.
- Last relevant evidence: C33 proves golden-equivalent Chat/Responses conversion from incremental frames at every byte split, long chunked streams, bare-JSON single-representation fallback, explicit 502 on truncation/oversized frames without partial success, and cancellation that drops the held upstream body. C31/C29/C32/C33 exact tests remain green; C34 proves 2000 text / 1000 tool / 500 reasoning deltas concatenate and free after done with parallel ordering preserved.
- Blocker: None; the prompt explicitly allows independent safe work when external harness/version evidence is unavailable.
- Next: C41 remove only confirmed-dead modules/dependencies, one group per commit.

## Material Decisions

- 2026-09-17: The user's explicit request to implement everything supersedes `docs/AGENT-PROMPT.md`'s single-checkpoint-per-invocation stopping rule; checkpoints still execute and commit sequentially in declared dependency order.
- 2026-09-17: Missing `PLAN.md`/`AUDIT-EVIDENCE.md` is recorded as unavailable source evidence, not replaced with invented content and not treated as a blocker to independently verifiable checkpoints.
- 2026-09-17: The machine-readable C00 contract is a sidecar manifest (`contracts/lean-proxy.json`); the Markdown contract explains migration and links exact source registries rather than duplicating mutable implementation catalogs as prose.

## Checkpoint History

- 2026-09-17: Goal frozen from the user request, `docs/AGENT-PROMPT.md`, and all C00-C42 entries in `docs/CHECKPOINTS.json`; C00 started.
- 2026-09-17: C00 passed. Added the human and machine-readable lean boundary, migration matrix, route/format inventories, source registry links, and contract tests. No runtime/configuration behavior changed; C01 is next.
- 2026-09-17: C01 passed. Added a reusable loopback scripted upstream, controlled stream gates/failures/request capture, OAuth-shaped rotation responses, native and translated fixtures, virtual-time/barrier primitives, and an owned temporary DB. No production behavior changed; C02 is next.
- 2026-09-17: C02 passed. The documented release benchmark measured the direct mock and full handler with equal successful SSE/tools work at cold/warm concurrency 1/8/32 over five repetitions, separately measured allocations, validated exact attempts and artifact completeness, and recorded predeclared thresholds. No runtime behavior changed; C03 is next.
- 2026-09-17: C03 passed. Removed Kiro semantic classification, full-success-body buffering, prompt mutation, and the hidden second generation; retained incremental AWS EventStream/tool translation, made malformed frames explicit errors, preserved the deprecated setting with a one-time warning, and verified first-event delivery plus cancellation against the loopback harness. C04 is next.
- 2026-09-17: C04 passed. Deleted process-wide Claude header capture/replay, threaded only current-request headers into the default adapter, enforced a non-secret allowlist with explicit provider defaults, and verified alternating plus concurrent account/session isolation. C05 is next.
- 2026-09-17: C05 passed. Deleted Kiro's process-wide frozen-msg0/system-prompt store without replacement, kept request-scoped protocol identifiers for C06, and verified current-only history across repeated sessions, compaction, account/model/system changes, tools, and concurrency. Real provider cache-hit/TTFT remains unverified because no permitted live Kiro credentials are available. C14 is next per the first-delivery order.
- 2026-09-17: C14 passed. Removed successful-generation and successful-web-fetch `Db::update` cleanup, preserved all legacy diagnostic bytes as historical/inert state, kept explicit narrow clearing, and proved repeated success on a large configuration neither published a new AppDb nor let future cooldown/degraded markers suppress routing. C15 is next.
- 2026-09-17: C15 passed. `Db::update` now performs one mutable full copy, shares old/new snapshots with blocking persistence, preserves Arc identity on no-op, publishes only after commit, and keeps commit/publication serialized even when the awaiting caller is cancelled. C06 is next as the first dependency-ready checkpoint after the frozen first-delivery sequence.
- 2026-09-17: C06 passed. Deleted both process-wide session/continuation maps without replacement; client ids remain authoritative, Antigravity preserves its UUID-plus-digits wire shape, OpenCode fallback is a stateless namespaced UUID, and Kiro continuation is deterministically bound to the selected configured connection while anonymous requests remain ephemeral. Five full-handler Kiro turns, adapter tests, 100,000-session churn, and cancellation prove zero retained entries; C07 is next.
- 2026-09-17: C07 passed. Split client-facing configured/native context metadata from the explicitly named transitional proxy rejection reader, preserved missing/empty/explicit/custom persisted values and canonical `opencode.source`, removed the unverified Codex overdrive claim, and rebuilt the dashboard with ownership-accurate copy. C08 is next.
- 2026-09-17: C08 passed. Deleted both ordinary-chat estimator calls and all synthetic context rejection/headroom code without changing advertised metadata or byte limits; preserved explicit count-tokens and made real non-retryable upstream errors retain their status/body. A large Unicode/tools/data-URL/base64 request reached the loopback upstream once without truncation. C09 is next.
- 2026-09-17: C09 passed. Same-format passthrough is now selected from source/target protocol capability after dynamic model metadata, independent of recognized User-Agent. Native Messages input no longer undergoes duplicate OpenAI preconversion, standalone Claude cache re-anchoring is deleted, client cache fields/unknown extensions survive, and incompatible pairs still translate. C10 is next.
- 2026-09-17: C10 passed. Deleted DefaultExecutor's same-URL retry loop and fixed sleeps for tokenrouter 429 plus 502/503/504, while preserving raw errors, distinct URL alternatives, pooled transports, and one separately identified transitional credential recovery. Controlled loopback counters show one request per unchanged URL; C11 is next.
- 2026-09-18: C11 passed. Deleted Codex's three-attempt first-SSE-error retry loop, fixed sleeps, and 256 KiB user-output window. A bounded one-event structured preflight now hands an initial overload/rate-limit failure to the request-scoped planner before commitment, while normal and post-delta events stay live and can never trigger another generation. C12 is next.
- 2026-09-18: C12 passed. Deleted Antigravity's three-attempt generation retry loop, Retry-After sleeps, jitter, and free-text body classifier. Raw errors now reach the request-scoped planner after one configured-endpoint request; pooled transport, protocol transformation, successful live SSE, and separate project/onboarding behavior remain. C13 is next.
- 2026-09-18: C13 passed. Added one request-scoped generation budget over eligible accounts and bounded protocol endpoint surfaces plus one auth follow-up; made chat the sole 401/403 recovery owner; stopped body/payload-invalid 400/413/422 account fan-out; removed Default and Mimo recovery multipliers; and preserved final raw errors, cancellation, pooled transports, and the no-post-commit rule. R3 is verified and C16 is next.
- 2026-09-18: C16 passed. Added an in-flight-only connection/generation refresh coordinator with canonical re-read, compare-before-persist, shared success/error, detached persistence-safe lifecycle, graceful drain, and zero idle entries. One hundred-way concurrency, same-token connection isolation, stale generation, failure, SQLite rollback, cancellation, delete/recreate, and shutdown are covered. Legacy callers remain untouched for C17A/C17B; C17A is next.
- 2026-09-18: C17A passed. Chat 401 and structured token-authentication 403 recovery plus the foreground Codex catalog helper now use the connection/generation coordinator and fresh canonical snapshots under C13's existing attempt budget. Sixteen parallel requests share one refresh, invalid grants remain one-per-request, permission 403 does not rotate, cancellation cannot lose issued tokens, and duplicate executor/CredentialManager refresh owners were removed. C17B is next.
- 2026-09-18: C17B passed. Proactive, quota/auto-ping, usage/reset-credit, manual configured-account, Kiro model-discovery, and proxy-aware connection-test refresh callers now share the connection/generation coordinator. Concurrent proactive, quota, and foreground recovery issue one refresh; stale background completion cannot overwrite a newer canonical pair; idle active state returns to zero. C18 is next.
- 2026-09-18: C18 passed. Deleted the token-keyed `RefreshDedup` completed-result/TTL map, old-token keys, `OnceCell` wrapper, dead provider refresh exports, and obsolete tests without replacement. Twenty thousand historical generations and 512 cancelled waiters leave zero idle state while existing same-generation singleflight remains correct. C19 is next.
- 2026-09-18: C19 passed. Replaced the models.dev read/refresh mutex with an ArcSwap-published immutable snapshot seeded from bundled validated Zen/Go metadata. Generation, `/api/catalog`, and `/v1/models` perform immediate local reads; only one bounded process task and explicit provider discovery refresh remotely, and failures preserve the prior Arc. Hung/failed refresh, atomic publication, cold start, enabled/custom/disabled rows, advertised limits, and canonical source are covered. C20 is next.
- 2026-09-18: C20 passed. Replaced Codex per-connection read/HTTP mutexes with an ArcSwap-published supporter index and pre-merged active union. Generation and metadata routes read synchronously; one bounded process task plus explicit discovery/test actions refresh remotely and publish atomically. Known chats ignore held refresh, failed refresh retains the prior Arc, inactive/deleted/identity-changed connections reconcile away, and cold unknown models cannot route to arbitrary accounts. C21 is next.
- 2026-09-18: C21 passed. Deleted the active connection-id TTL project cache and the proven-dead token-keyed duplicate, consolidated canonical/legacy project parsing, and made Antigravity generation read only the selected connection snapshot with no loadCodeAssist or project lock. Concurrent first uses stay local, delete/recreate cannot restore old metadata, and setup discovery failure is explicit while credentials remain durable. C22 is next.
- 2026-09-18: C22 passed. Deleted Antigravity onboarding polling/spawn from generation and replaced setup polling with one active-only connection/generation lifecycle using pooled proxy-aware transport, bounded attempts, readiness diagnostics, explicit test retry, delete cancellation, and shutdown drain. One hundred chats start zero onboarding workers; C23 is next.
- 2026-09-18: C23 passed. Deleted Claude quota's access-token-keyed five-minute result/error cache, stale-on-error replay, and ineffective OnceCell map without replacement. Explicit success/error/cancellation and twenty-four concurrent callers retain no historical or in-flight state; dashboard polling remains sixty-second and hidden-page-aware. C24 is next.
- 2026-09-18: C24 passed. Default/missing health settings no longer start provider probes, literal true remains explicit opt-in, `/health` is local liveness with honest unknown/stale timestamps, manual connection tests publish diagnostics, legacy fields remain routing-inert, and the write-only circuit breaker was deleted without touching security controls. C25 is next.
- 2026-09-18: C25 passed. Quota auto-ping now has an AppState-owned active-only lifecycle: empty configuration creates no sleeper, runtime enable starts one shared Claude/Codex/GLM worker, final disable wakes it, shutdown drains it, and restart preserves opt-in plus ping markers. Actual pings were already opt-in before this change; proactive OAuth refresh remains independent. R4 is verified and C26 is next.
- 2026-09-18: C26 passed. The HTTP handler now moves its parsed JSON into the sole planning/translation consumer instead of deep-cloning it there; the account planner still retains one immutable source and per-attempt clones required by C13 fallback. Native/translated/large/fallback/cancellation semantics stayed green, and the equal-work release allocator measured 19,188 fewer bytes/request at warm concurrency 32. C27 is next.
- 2026-09-18: C27 passed. DefaultExecutor now transforms and serializes request JSON once through a checked 64 MiB writer, then shares the request-scoped bytes across identical account/auth attempts while rebuilding headers, URL, proxy, and pooled transport. Hyper and Reqwest preserve identical UTF-8 JSON, content type, Unicode, tools, and unknown fields; changed model/body rebuilds, payload-limit errors are terminal, and no cross-request cache exists. The single-attempt allocation cell stayed effectively flat at 269,135 bytes/request; C28 is next.
- 2026-09-18: C28 passed. Removed full transformed request JSON from all executor response/result structures, debug implementations, constructors, server wrappers, and `PreparedUpstreamBody`; URL/header/transport metadata and bounded shared bytes remain. CLI `route --json` recomputes the DefaultExecutor transform locally after success. Source guards plus a 4 MiB held-EOF SSE test prove the response graph has no request `Value` while tool, usage, cancellation, one-attempt, fallback, raw-error, and translator behavior remain intact. R5 stays pending through C34; C29 is next.
- 2026-09-18: C29 passed. Added one checked Reqwest/Hyper body reader with independently configurable 64 MiB success/protocol and 1 MiB diagnostic defaults, enforcing yielded/decompressed bytes rather than trusting Content-Length; only identity-encoded lengths can reject early. Oversized or partial successes fail before commitment; diagnostics retain status/Retry-After with explicit in-budget markers. All verified generation executor collectors, including Grok Web NDJSON, use the primitive; dead `UpstreamResponse::text` is gone, and native SSE/Codex preflight/C13 behavior remain unchanged. C30A is next.
- 2026-09-18: C30A passed. Native OpenAI/Claude passthrough performs no image fetch, while already-inline-required chat targets and Codex's existing remote image parts use one request-scoped checked stream reader. Defaults bound decoded bytes to 10 MiB/image and 32 MiB/request, encoded data URLs to 48 MiB/request, and final JSON to the 64 MiB C27 ceiling. Base64 writes directly into one pre-sized string; Claude shape and Codex data/detail/order survive. Typed 413 budget and 502 fetch/validation failures stop before generation, with no URL/text fallback, cache, timer, or C30B connect-pin change. C30B is next.
- 2026-09-18: C30B passed. Every image hostname and redirect hop now pins all validated public DNS candidates into Reqwest's actual connector while retaining the original URL for Host, TLS SNI, and certificate-name verification. Private/reserved IPv4/IPv6 and mapped addresses are blocked, system proxies cannot independently re-resolve, connect failure has no unvalidated DNS fallback, and no normal pool policy or C30A bound was relaxed. Deterministic code-risk tests cover host preservation, multi-address connect, TLS name mismatch, and private destinations; C31 is next.
- 2026-09-18: C31 passed after follow-up audit. Forced SSE collapse, Ollama/Gemini/Kiro/CommandCode registry paths, Responses/Messages compatibility, Kiro binary ordering, and Cursor full-response accumulators now validate non-negative bounded wire indices before allocation, preserve bounded deterministic order, and cap choices, calls, per-tool arguments, and total retained text/reasoning/tool state with checked growth. Forced Responses parse/final assembly share one budget; args-before-identity are preserved and missing identity at finish fails; recorded transform failure suppresses finish output. Unsupported indices, fake tool identity, unknown argument targets, and limit overflow fail explicitly; the forced collector returns 502 before commitment without JSON fallback masking, while committed translations terminate with a structured error. Exact-limit/out-of-order/repeated/Unicode goldens and cancellation remain valid; C32 is next.
- 2026-09-18: C32 passed after follow-up audit. Added one bytes-first bounded SSE/line cursor with a 1 MiB positive-configurable incomplete-record limit, checked growth, monotonic scanning, complete-frame UTF-8, multiline fields, EOF dispatch, and tiny-tail capacity compaction. A single format-aware live dispatch now feeds usage/completion plus translation or dashboard consumers for all text protocols; CommandCode handles both NDJSON and its executor SSE envelope. Pivot stage two receives OpenAI payloads, non-stream EOF is flushed, collected Responses validates without marker sniffing, valid output precedes terminal failure, and explicit native Responses/Messages routes preserve exact bytes unless proxy-injected search items still require sanitization. Dashboard conversion keys off the resolved upstream format rather than a configurable provider name, and per-frame usage observation inspects only newly emitted output. Pre-commit overflow is 502, post-commit overflow is one terminal event, cancellation remains intact, and Codex plus binary Kiro/Cursor framing stay separate. C33 is next.
- 2026-09-18: C33 passed. Forced SSE-to-JSON now feeds completed C32 frames incrementally into a request-local C31-bounded accumulator without retaining full raw SSE history. Wire bytes remain counted against the C29 success limit with declared-length early rejection; oversized, truncated, framing, and state failures are explicit pre-commit 502 without partial JSON or fallback masking. Bare non-SSE JSON uses only its short pre-first-frame prefix; cancellation drops the held upstream body and native streaming is unchanged. C34 is next.

## Completion

- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
