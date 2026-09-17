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
  - Status: pending
  - Evidence:

- R5: Reduce payload copies and bound all collected response, image, tool, and SSE state without changing valid wire semantics (C26-C34).
  - Source: `docs/CHECKPOINTS.json` C26-C34.
  - Acceptance: Every checkpoint acceptance statement C26-C34 is met; native streams remain streaming and valid golden payloads remain equivalent.
  - Primary evidence: Ownership/serialization instrumentation, byte-bound edge tests, all-chunk-boundary SSE tests, translator goldens, and allocation measurements.
  - Status: pending
  - Evidence:

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

- Closes: R4 / C17B.
- Smallest next action: Migrate manual, proactive, quota, catalog/model-discovery, and connection-test OAuth refresh callers to the same connection/generation coordinator without losing provider-specific proxy behavior.
- Expected evidence: Foreground 401, proactive refresh, model/catalog or quota access race on one generation and share one refresh; no legacy direct dispatch or token-specific refresh bypass remains outside the coordinator.
- Stop or replan if: A control-plane operation has no stable configured connection identity or requires a proxy-aware provider refresh seam; isolate that caller rather than bypassing coordination.

## Current State

- Resolved: R1 / C00, R2 / C01-C02, R3 / C03-C14, plus C15-C17A within R4. Proxy-owned semantic/header/history replay, heuristic context policy, client-identity passthrough gating, all three executor temporal retry schedulers, and successful-response legacy housekeeping are gone. C13 provides the single bounded request-scoped generation/account/auth planner; C16 provides the connection-scoped refresh service; C17A routes all foreground recovery through it and removes duplicate executor/manager owners.
- Last relevant evidence: Sixteen simultaneous foreground 401s share one token refresh and each consume exactly one counted follow-up generation; invalid-grant, structured non-token 403, waiter cancellation, canonical publication, and the C13 budget remain covered. Codex catalog refresh uses the same coordinator.
- Blocker: None; the prompt explicitly allows independent safe work when external harness/version evidence is unavailable.
- Next: C17B control/background OAuth caller migration.

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
- 2026-09-18: C13 passed. Added one request-scoped generation budget over eligible accounts and bounded protocol endpoint surfaces plus one auth follow-up; made chat the sole 401/403 recovery owner; stopped 400/422 account fan-out; removed Default and Mimo recovery multipliers; and preserved final raw errors, cancellation, pooled transports, and the no-post-commit rule. R3 is verified and C16 is next.
- 2026-09-18: C16 passed. Added an in-flight-only connection/generation refresh coordinator with canonical re-read, compare-before-persist, shared success/error, detached persistence-safe lifecycle, graceful drain, and zero idle entries. One hundred-way concurrency, same-token connection isolation, stale generation, failure, SQLite rollback, cancellation, delete/recreate, and shutdown are covered. Legacy callers remain untouched for C17A/C17B; C17A is next.
- 2026-09-18: C17A passed. Chat 401 and structured token-authentication 403 recovery plus the foreground Codex catalog helper now use the connection/generation coordinator and fresh canonical snapshots under C13's existing attempt budget. Sixteen parallel requests share one refresh, invalid grants remain one-per-request, permission 403 does not rotate, cancellation cannot lose issued tokens, and duplicate executor/CredentialManager refresh owners were removed. C17B is next.

## Completion

- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
