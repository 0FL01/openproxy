# Goal: Reliable Codex search over Responses

> Superseded 2026-09-19 by [`2026-09-19-codex-web-mcp.md`](2026-09-19-codex-web-mcp.md): hosted search now uses the direct authenticated MCP tool.

Status: complete
Source: User-reported OpenCode `invalid web_search` and buffered streaming,
followed by approved implementation instruction (2026-09-14)
Last updated: 2026-09-14

## Objective
Make proxy-injected Codex hosted search work correctly with OpenCode's
`@ai-sdk/openai` Responses client: no unavailable local-tool error and genuine
incremental SSE delivery.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and
Primary Evidence. Work on the smallest unresolved outcome. Do not add
requirements from reviews, tests, tools, speculative risks, or optional source
text. Finish when every required outcome is resolved and affected constraints
remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Proxy-injected hosted search is not exposed as an unavailable OpenCode
  local tool.
  - Source: User's observed `Model tried to call unavailable tool 'web_search'`.
  - Acceptance: For search actually injected by OpenProxy, Responses output
    omits hosted `web_search_call` tool lifecycle/items while preserving
    reasoning, text, URL annotations, usage, and terminal events. Client-native
    hosted tools remain untouched.
  - Primary evidence: Fragmented native Responses SSE regression tests.
  - Status: verified
  - Evidence: Fragmented SSE and non-streaming fixtures remove only hosted
    `web_search_call` lifecycle/items while retaining reasoning, text, URL
    annotations, usage, and one terminal response.

- R2: `/v1/responses` streaming is incremental rather than full-body buffered.
  - Source: User reports seeing intermediate output only after the final answer.
  - Acceptance: The converter returns and emits the first complete SSE frame
    before the upstream body completes, including with fragmented frames.
  - Primary evidence: Deterministic delayed-body stream test.
  - Status: verified
  - Evidence: The delayed-body regression failed before the edit and now proves
    the converter returns and emits `response.created` while upstream remains
    blocked.

- R3: Existing Codex search isolation and opt-in behavior remain intact.
  - Source: Original requirement and latest instruction to make a reliable fix,
    not replace the design.
  - Acceptance: Only an effective direct Codex/account-capable header injection
    receives response sanitization; native requests and non-Codex routes are
    not reclassified.
  - Primary evidence: Focused marker and passthrough tests plus existing search
    isolation tests.
  - Status: verified
  - Evidence: Marker tests prove provenance is set only for proxy injection;
    the native hosted-search fixture passes through all search events unchanged.

- R4: The offline-verified fix is committed, pushed, deployed, and healthy.
  - Source: User instruction to commit, push, and deploy.
  - Acceptance: `main` equals `origin/main`, production runs the new healthy
    image, and no live model request is made during verification.
  - Primary evidence: Git refs, Docker build/deploy, and `/health`.
  - Status: verified
  - Evidence: Runtime commit `6b3a1272` is pushed and deployed as image
    `sha256:07a5807aab4`; container and `/health` are healthy. Verification made
    no model, Codex, OpenCode, or search request.

### Constraints
- C1: Keep `@ai-sdk/openai`; do not switch OpenCode to Chat Completions.
- C2: Do not perform a live OpenCode or Codex canary; the user will test it.
- C3: Preserve existing client tools, native hosted-tool fidelity, citations,
  usage tracking, cache bypass, account gating, and auto-ping isolation.
- C4: Do not log or commit credentials, prompts, search queries, or results.

### Non-goals
- OpenCode plugin/MCP/standalone search or a temporary compatibility mode.
- Rendering hosted search as an OpenCode-local tool.
- Combo/fusion search policy, filters, locations, or provider policy storage.
- Refactoring unrelated Messages/Chat translators.

## Change Envelope
- Target: Effective-injection response marker, incremental Responses SSE
  conversion, and narrowly scoped hosted-search event sanitization.
- Expected paths, symbols, and direct consumers: `src/server/api/chat.rs`,
  `src/server/api/compat.rs`, and their nearest module/integration tests.
- Allowed and forbidden artifacts: Existing Axum body streams, response
  extensions, and current translators only; no dependency, migration, endpoint,
  service, plugin, or persistent state.
- User or harness budget: Offline tests only; then commit/push/deploy and health
  verification.

## Current Checkpoint
- Closes: None; objective complete.
- Smallest next action: Stop.
- Expected evidence: All required outcomes are verified below.
- Stop or replan if: Not applicable.

## Current State
- Resolved: R1-R4.
- Last relevant evidence: Runtime commit deployed healthy without a model
  request; local offline evidence covers incremental delivery and scoped event
  sanitization.
- Blocker: None.
- Next: None.

## Material Decisions
- 2026-09-14: `invalid` is a client validation artifact after successful hosted
  execution; the proxy must hide only tool lifecycle it injected invisibly.
- 2026-09-14: Use an in-process response extension to carry effective injection
  provenance; do not expose a wire header or infer the client.
- 2026-09-14: Preserve native Responses fidelity when the client itself declares
  the hosted tool.

## Checkpoint History
- 2026-09-14: Goal created after RECON; implementation not started.
- 2026-09-14: Effective-injection provenance, incremental Responses conversion,
  and scoped search-event sanitization implemented; focused offline tests pass.
- 2026-09-14: Commit gates passed: 1,940 library tests, fmt, and clippy. The
  existing `responses_compact_normalizes_input_and_sets_compact_flag`
  integration still returns a wiremock 404 before the changed response
  conversion path; the other three tests in that file pass.
- 2026-09-14: Runtime commit `6b3a1272` pushed and deployed; production image
  and `/health` verified without a live model request.

## Completion
- Resolved outcomes: R1-R4.
- Commands and artifacts: Deterministic red/green delayed-stream regression;
  fragmented injected/native SSE fixtures; non-stream fixture; 16 compat tests;
  1,940-test library suite; fmt; clippy; Docker production build/deploy; health
  and git-ref checks.
- Constraint and diff-scope check: Kept `@ai-sdk/openai`; no live canary,
  dependency, migration, service, endpoint, persistent state, plugin, client
  detection, credential/query/result logging, or non-Codex search expansion.
- Final status: complete.
