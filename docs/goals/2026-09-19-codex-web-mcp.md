# Goal: Codex web search over MCP

Status: complete
Source: User-approved audited MCP migration plan and instruction to implement, commit, push, and deploy (2026-09-19)
Last updated: 2026-09-19

## Objective
OpenCode uses one authenticated stateless MCP tool, `codex_web_search`, for Codex hosted web search; OpenProxy reuses one Codex routing/fallback and bounded Responses collection path, preserves every ordered answer and citation, removes the former public/legacy search surfaces, and is committed, pushed, deployed, and healthy.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: OpenProxy exposes one direct remote MCP endpoint for Codex web search.
  - Source: User-approved audited plan.
  - Acceptance: Authenticated `POST /v1/mcp` implements MCP 2025-11-25 `initialize`, initialized/cancelled notifications, `ping`, `tools/list`, and `tools/call`; it exposes only `codex_web_search`, is stateless, rejects browser `Origin`, and has no SSE/session/alias surface.
  - Primary evidence: Focused MCP route integration test plus OpenCode 1.18.31 discovery smoke with temporary config.
  - Status: verified
  - Evidence: `codex_web_mcp_api` passes lifecycle/auth/origin/GET/tool-list tests, and installed OpenCode 1.18.31 connects to the temporary direct remote server and discovers `codex_web` without a search call.

- R2: MCP search uses the existing Codex generation owner and returns complete bounded results.
  - Source: User requirements for one logic without duplication and correct display for dozens of results.
  - Acceptance: The MCP path reuses catalog/supporter routing, pooled transport, OAuth refresh, account fallback, admission, and one incremental Responses accumulator; successful output contains every ordered final message and exact-URL-deduplicated citation in one text block, while terminal/limit failures return no partial answer.
  - Primary evidence: One mocked MCP search integration plus one many-item accumulator regression and one terminal-failure table test.
  - Status: verified
  - Evidence: Shared incremental Responses accumulator preserves ordered native output/citations, merges terminal output, enforces 512 items, stops on terminal events, and passes many-item/terminal-failure tests; mocked MCP execution proves account fallback and one final text block.

- R3: Search model selection is Luna-only and deterministic.
  - Source: User clarification to prefer `gpt-5.6-luna` or another current Luna rather than older `gpt-5.5`.
  - Acceptance: OpenProxy prefers published search-capable `gpt-5.6-luna`, otherwise the highest-version published search-capable Luna with active supporters; it never automatically falls back to a non-Luna model and returns an actionable tool error when no eligible Luna exists.
  - Primary evidence: Focused model-selection unit test and captured mocked upstream model.
  - Status: verified
  - Evidence: Selector unit test proves exact `gpt-5.6-luna` preference, highest supported Luna fallback, and no non-Luna result; routed MCP fixture ignores local `default_model: gpt-5.5` and sends `gpt-5.6-luna` on both account attempts.

- R4: Public Codex native search and all owned legacy search configuration are removed.
  - Source: User requirement for migration without backward compatibility or legacy tails.
  - Acceptance: External Codex requests containing `tools[].type == "web_search"` fail with `codex_web_search_requires_mcp` before upstream regardless of `tool_choice`; obsolete header/settings/UI/runtime helpers are gone, and the exact persisted `codexWebSearchContextSize` key is removed without changing unrelated settings.
  - Primary evidence: Public route rejection regression, settings migration regression, and repository legacy-symbol audit.
  - Status: verified
  - Evidence: Public `/v1/responses` regression rejects native search with `codex_web_search_requires_mcp` even under `tool_choice:none`; legacy symbols remain only in the exact migration and absence assertions, and settings normalization preserves an unrelated key.

- R5: OpenCode configuration installs the direct MCP server with the same real OpenProxy API key.
  - Source: User configuration example and no-client-wrapper requirement.
  - Acceptance: Apply/manual config writes `mcp.codex_web` at `<LUDKA2_API_URL>/mcp`, `oauth:false`, `enabled:true`, timeout 300000, and the same non-placeholder API key; update/reset preserves unrelated MCP entries and removes only the owned entry at the final OpenProxy model lifecycle boundary.
  - Primary evidence: Existing OpenCode native-settings regression extended for apply/update/reset plus dashboard build.
  - Status: verified
  - Evidence: OpenCode native-settings regression proves same URL/key, `oauth:false`, timeout 300000, unrelated MCP preservation, owned reset, and missing-key rejection; dashboard build completes 81 pages.

- R6: Repository contracts and current documentation describe the new narrow ownership exception.
  - Source: Approved plan and existing frozen lean-proxy contract.
  - Acceptance: Lean contract/manifest, architecture, agent guide, route index, README, and plugin setup agree that `/v1/mcp` is one stateless credential-bound search tool while general tool execution remains client-owned; historical goals are marked superseded rather than rewritten.
  - Primary evidence: Contract tests, JSON parse, and diff review.
  - Status: verified
  - Evidence: Lean Markdown/JSON, C44 checkpoint/results, architecture, agent guide, route index, README/plugin setup, C39 boundary test, and four historical supersession notices agree; both JSON documents parse.

- R7: The verified change is committed, pushed, deployed, and healthy.
  - Source: User instruction to commit, push, and deploy.
  - Acceptance: Current branch refs match origin, production Compose runs the built image, and `/health` returns `status: ok`.
  - Primary evidence: Git refs, Compose status, and health response.
  - Status: verified
  - Evidence: Runtime commit `3bd196ea` is pushed to `origin/perf/lean-proxy-plan` and deployed as image `sha256:35849359115465d69d40c702a61e1c7969576a3fa5a3dcc6b6d0b4792931bec6`; Compose reports healthy and `/health` returns `status: ok`.

### Constraints
- C1: No new dependency, client wrapper, local MCP process, MCP OAuth, MCP session store, cache, background worker, or second Codex HTTP/routing/fallback path.
- C2: The only MCP tool arguments are `query` and optional `response_length` (`short|medium|long`, default `medium`), mapped to Codex search context `low|medium|high`.
- C3: Search output is all-or-error within existing bounded streaming state and a 16 MiB serialized MCP result; no silent truncation, raw SSE, reasoning, or search internals.
- C4: `/v1/web/fetch`, non-Codex provider tools, ordinary function/tool-result forwarding, model discovery, and unrelated settings remain unchanged.
- C5: OpenCode uses one 300000 ms remote-server timeout; server execution is bounded below it and metadata requests do not consume generation admission.

### Non-goals
- General MCP framework, additional tools, resources, prompts, subscriptions, batches, resumability, or SSE transport.
- Client history, compaction, tool loops, query planning, semantic repair, generation retry, or paid live search in CI.
- New search-model setting, `model` tool argument, dashboard search toggle, or generic removal of other providers' web-search types.
- `/v1/web/fetch` changes, JSONC editor refactor, or broad unrelated cleanup.

## Change Envelope
- Target: One `POST /v1/mcp` transport, trusted internal Codex search provenance, shared bounded Responses collection/projection, Luna selection, public hard cutover, owned OpenCode configuration, exact legacy settings cleanup, and matching contracts/tests/docs.
- Expected paths, symbols, and direct consumers: `src/core/chat/stream_to_json.rs`, `src/server/api/chat.rs`, `src/server/api/codex_web_mcp.rs`, `src/server/api/mod.rs`, `src/server/api/compat.rs`, `src/server/api/admission.rs`, `src/server/codex_catalog.rs`, `src/server/api/cli_tools.rs`, `src/types/mod.rs` and the existing persistence normalization path, `web/src/components/cli-tools/OpenCodeToolCard.tsx`, nearest integration/contract tests, `contracts/lean-proxy.*`, architecture/setup docs, and historical goal headers.
- Allowed and forbidden artifacts: Existing Axum/Serde/Tokio/HTTP/Codex mechanisms only; no dependency, schema framework, service, wrapper, generic MCP layer, duplicate parser, duplicate client, or unrelated refactor.
- User or harness budget: Minimum direct regression evidence; no real paid search in CI; commit, push, production Compose deploy, and health check after closure.

## Approved Implementation Plan

1. Extend the existing Responses accumulator before lossy Chat conversion. Preserve native output items, annotations, terminal state, usage, and errors; treat only `response.completed` as success; stop at terminal events; support completed native bare JSON; use a dedicated 512-item Responses bound while retaining frame/state/index limits.
2. Add a small handwritten stateless MCP 2025-11-25 dispatcher at `POST /v1/mcp`. Apply Origin rejection, unconditional existing API-key validation, 64 KiB request limit, exact JSON-RPC IDs/errors, empty 202 notification responses, JSON request responses, GET 405, and no session header or aliases.
3. Keep the existing provider fallback owner responsible for supporters, attempt order/budget, OAuth recovery, proxies, pooled transport, fallback classification, and logs. Separate upstream attempt execution from downstream presentation only as much as required so HTTP and MCP consume the same result; do not call OpenProxy over loopback or invoke `CodexExecutor` directly from MCP.
4. Add non-wire internal provenance (`External` versus `InternalMcp`). Reject any external physical-Codex request containing native `web_search` before account work, including `tool_choice:"none"`; allow only the MCP-constructed request.
5. Select `gpt-5.6-luna` when its published catalog entry advertises search and has active supporters. Otherwise select the highest parsed version in the published search-capable Luna family; never fall back to non-Luna; then use normal account fallback for the chosen model.
6. Project successful native Responses output into exactly one MCP text block. Iterate output items numerically, concatenate parts within each message, separate messages by blank lines, collect HTTP(S) URL citations in annotation order, deduplicate exact URLs, append one Sources section, and return all-or-error at 16 MiB. Never expose reasoning, raw search calls, raw SSE, or partial output on failure.
7. Remove the old public flags, depth setting/API/UI, retired header constant/cleaner, hard-coded false status, and stale frontend setter. Remove the exact persisted depth key while preserving unrelated flattened settings; do not scrub arbitrary user-owned external config.
8. Write one owned `mcp.codex_web` OpenCode entry using the same normalized `/v1` URL plus `/mcp` and the same real API key as the provider. Preserve unrelated MCP entries; remove the owned entry only when the final OpenProxy model/config is removed. Manual config must match; no wrapper or toggle.
9. Update the frozen lean boundary and current docs, add superseded notices to historical search goals, and replace stale contract assertions with the narrow two-tool-route/no-agent-loop boundary.
10. Verify with focused protocol/auth, mapping/execution, many-result, terminal-failure, public-cutover, settings/config, contract, dashboard/plugin, library/fmt/clippy, real OpenCode discovery, then commit, push, deploy, and health-check.

## Current Checkpoint
- Closes: None; objective complete.
- Smallest next action: Stop.
- Expected evidence: All required outcomes are verified below.
- Stop or replan if: Not applicable.

## Current State
- Resolved: R1-R7.
- Last relevant evidence: Runtime commit is pushed and deployed; Compose and `/health` are healthy.
- Blocker: None.
- Next: None.

## Material Decisions
- 2026-09-19: Use handwritten stateless MCP 2025-11-25 over JSON responses; no MCP or JSONC dependency.
- 2026-09-19: Reuse existing OpenProxy API-key validation unconditionally for MCP; OpenCode sends the key as Bearer.
- 2026-09-19: Preserve native Responses state once and derive both existing HTTP presentation and MCP search projection from it.
- 2026-09-19: Prefer `gpt-5.6-luna`, fall back only within the published search-capable Luna family, and never select `gpt-5.5` automatically.
- 2026-09-19: Remove the exact owned persisted depth key, but do not destructively scrub arbitrary user-owned OpenCode files.

## Checkpoint History
- 2026-09-19: Goal created; R1-R7 frozen; implementation not started.
- 2026-09-19: R2 foundation passed: one bounded Responses parser now feeds HTTP and MCP projections; dozens of ordered messages/citations and terminal all-or-error behavior are covered. MCP lifecycle/auth and public hard-cut tests also pass. Next is one mocked routed tool call.
- 2026-09-19: R1-R6 verified. Mocked routing proves Luna-only selection, short→low mapping, account fallback, private credentials, and final citations; OpenCode configuration/contracts/docs are aligned and real OpenCode discovery connects without a paid call. Next is R7 commit/deploy closure.
- 2026-09-19: R7 verified. Runtime commit `3bd196ea` was pushed and deployed as image `sha256:358493591154`; production Compose and `/health` are healthy.

## Completion
- Resolved outcomes: R1-R7.
- Commands and artifacts: 1,026 library tests; focused MCP, Codex C11/C20, forced-stream C33, admission C36, settings/config, and C39 tests; real OpenCode discovery; dashboard/plugin builds; JSON, fmt, clippy, production Docker build, and `/health`.
- Constraint and diff-scope check: No dependency, wrapper, session, cache, worker, duplicate Codex execution path, paid CI search, `/v1/web/fetch` change, or unrelated configuration cleanup was added.
- Final status: complete.
