# Goal: Codex standalone indexed search

Status: complete
Source: User report comparing the deployed MCP search with `mateusdcc/codex-search-opencode`, approved instruction to implement, commit, push, and deploy (2026-09-19)
Last updated: 2026-09-19

## Objective
`codex_web_search` returns bounded indexed results from Codex's standalone search endpoint without a Luna/Responses generation turn, while preserving OpenProxy authentication, private Codex credentials, account fallback, the public native-search cutover, and the existing one-tool stateless MCP surface.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: MCP search uses the Codex standalone indexed-search endpoint.
  - Source: User-provided reference implementation and correction request.
  - Acceptance: A valid tool call sends one bounded JSON request to `/backend-api/codex/alpha/search` with `model: "gpt-4o"`, one `commands.search_query`, and the requested `short|medium|long`; it sends no request to `/codex/responses` and performs no Luna/model-catalog selection.
  - Primary evidence: Mocked upstream request-capture regression covering all three response lengths.
  - Status: verified
  - Evidence: C20 captures `/backend-api/codex/alpha/search` for `short`, `medium`, and `long`; every body has `model: "gpt-4o"`, one query, no tools/tool choice, and the catalog receives zero requests.

- R2: Standalone search preserves private credential routing and bounded recovery.
  - Source: Existing OpenProxy account-management contract and approved correction plan.
  - Acceptance: The incoming OpenProxy API key authenticates MCP only; active Codex accounts supply upstream bearer/account headers in priority order, use configured proxy/client pooling, coordinated OAuth recovery, and bounded transient/account fallback.
  - Primary evidence: One mocked first-account failure/second-account success regression plus credential-header assertions.
  - Status: verified
  - Evidence: C20 proves priority-ordered first-account 429 fallback to the second account, private per-account bearer/account headers, and no forwarding of the incoming OpenProxy key; the runtime reuses the configured proxy, client pool, and refresh coordinator.

- R3: MCP returns the indexed response without an OpenProxy model-generated synthesis.
  - Source: User requirement for the faster and more reliable indexed-search behavior shown by the reference repository.
  - Acceptance: Endpoint `output` and structured `results` are normalized into one MCP text block with ordered titles, snippets, URLs, and citation references; malformed, failed, timed-out, or oversized responses return an all-or-error result with no partial output.
  - Primary evidence: A many-result normalization fixture and error table.
  - Status: verified
  - Evidence: Unit fixtures preserve 40 ordered Unicode results/citations and reject malformed, empty, or over-512 result sets; MCP integration returns one text block with no structured content.

- R4: The obsolete MCP Luna/Responses path and its owned contract/config tails are removed.
  - Source: User correction and no-duplication requirement.
  - Acceptance: MCP-only Luna selection, internal generation provenance/result handling, native-search request construction, Responses search projection, and 300-second client timeout are gone; public native Codex search remains rejected and ordinary Responses behavior remains intact.
  - Primary evidence: Source audit, public cutover regression, OpenCode config regression, and existing forced-Responses tests.
  - Status: verified
  - Evidence: Source audit and C39 prove the MCP path owns no Luna/catalog/Responses/SSE path; C11, C33, public boundary coverage, and OpenCode config tests pass with a 30-second client timeout.

- R5: Real OpenCode calls prove `short`, `medium`, and `long` work against non-example queries.
  - Source: User instruction to test normal live searches at all three levels.
  - Acceptance: Installed OpenCode invokes `codex_web_search` once at each level, every call completes within the 30-second client timeout, returns non-empty sourced output, and does not report MCP timeout.
  - Primary evidence: Three explicit post-deploy OpenCode tool calls using current facts and official-source queries.
  - Status: verified
  - Evidence: Post-deploy `codex_web_search` calls at `short`, `medium`, and `long` all completed under the configured 30-second OpenCode timeout with non-empty indexed results and real source URLs for Rust releases, the MCP specification, and OpenAI GPT-5.1 documentation.

- R6: The verified correction is committed, pushed, deployed, and healthy.
  - Source: User instruction to commit, push, and deploy.
  - Acceptance: Only correction-owned changes are committed, branch refs match origin, production Compose runs the corrected image, and `/health` returns `status: ok`.
  - Primary evidence: Staged diff review, Git refs, Compose status, and health response.
  - Status: verified
  - Evidence: Correction commits `6c8d276b` and `0e0007b1` are pushed to `origin/perf/lean-proxy-plan`; clean commit `0e0007b1` was built as image `sha256:5ff434404671`, Compose reports healthy, and `/health` returns `status: ok`.

### Constraints
- C1: Keep `POST /v1/mcp`, server key `codex_web`, OpenCode tool `codex_web_search`, unconditional OpenProxy API-key auth, stateless transport, and the exact `query` plus optional `response_length` input surface.
- C2: No client plugin/wrapper, MCP SDK, new dependency, search session, `open/find/click`, new service, cache, worker, or generic search framework.
- C3: The upstream endpoint is undocumented `alpha/search`; isolate it behind one provider-specific transport and record that stability risk without exposing private credentials.
- C4: Preserve `/v1/web/fetch`, public native-search rejection, ordinary Codex generation, shared Responses bounds, and unrelated provider/tool behavior.
- C5: Preserve all pre-existing dirty-worktree changes and stage only correction-owned hunks.

### Non-goals
- Full `codex-research` session continuity or legacy plugin aliases.
- Recency/domain filters, multiple tools, or model selection.
- Reworking unrelated CLI-tool removals currently present in the worktree.
- Paid/live search in automated CI.

## Change Envelope
- Target: Provider-private standalone search transport, Codex-account execution/fallback, indexed response normalization, MCP adapter switch, obsolete MCP generation cleanup, 30-second OpenCode config, focused contracts/docs/tests, deployment.
- Expected paths, symbols, and direct consumers: `src/core/executor/codex.rs` or one adjacent provider-private module, `src/server/api/codex_web_mcp.rs`, `src/server/api/chat.rs`, `src/core/chat/stream_to_json.rs`, OpenCode config/status code, nearest MCP/Codex/contract tests, lean contracts, architecture/README/agent docs, and historical supersession notices.
- Allowed and forbidden artifacts: Existing Reqwest/Axum/Serde/client-pool/proxy/OAuth/logging mechanisms only; no dependency, schema, session store, wrapper, duplicate general router, or unrelated cleanup.
- User or harness budget: Minimal direct evidence, three manual live calls after deploy, commit, push, production Compose deploy, and health check.

## Approved Implementation Plan
1. Add one bounded provider-private call to `https://chatgpt.com/backend-api/codex/alpha/search` using the selected connection's private bearer and ChatGPT account ID, current client pool/proxy, request ID, `model: "gpt-4o"`, one `search_query`, and direct `response_length`.
2. Execute it across active Codex accounts in `(priority, id)` order with one coordinated OAuth recovery, bounded 502/503/504 retries, account fallback for retryable failures, terminal request errors, and metadata-only attempt logging.
3. Normalize `{output, results}` directly. Preserve endpoint output, map deterministic ref IDs to plain citations, append ordered sources, fall back to ordered title/URL/snippet results, and enforce a 16 MiB all-or-error response bound.
4. Keep MCP protocol/auth/admission unchanged, but replace the Luna/Responses call with standalone search and reduce server/client deadlines to fit the reference's 15-second search timeout and OpenCode's 30-second MCP timeout.
5. Remove MCP-only Luna selection, catalog capability checks, internal generation provenance/result branches, forced-SSE search collector, and Responses-search projector while retaining ordinary Responses fixes and public native-search rejection.
6. Replace the old Luna fixture with exact standalone request/fallback/output tests, run focused Rust/OpenCode/dashboard/plugin gates, then perform three real OpenCode searches, commit only correction-owned hunks, push, deploy, and health-check.

## Current Checkpoint
- Closes: None; objective complete.
- Smallest next action: Stop.
- Expected evidence: All required outcomes are verified below.
- Stop or replan if: Not applicable.

## Current State
- Resolved: R1-R6.
- Last relevant evidence: The clean committed image is healthy and all three live OpenCode search lengths return sourced indexed output without timeout or private citation markers.
- Blocker: None.
- Next: None.

## Material Decisions
- 2026-09-19: Treat `model: "gpt-4o"` as the standalone search protocol selector, not an OpenProxy generation model.
- 2026-09-19: Preserve the existing stateless one-query MCP schema; do not import the reference plugin's sessionful research surface.
- 2026-09-19: Return standalone indexed output directly instead of asking Luna to synthesize an answer.

## Checkpoint History
- 2026-09-19: Goal created from completed RECON; R1-R6 frozen; implementation not started.
- 2026-09-19: R1-R4 verified. MCP now uses bounded standalone indexed search with private account fallback; the prior Luna/Responses path and 300-second config are removed. Focused, library, dashboard, plugin, formatting, and contract gates pass.
- 2026-09-19: Initial live calls exposed raw private citation markers in long output; focused normalization was added and redeployed. Final `short|medium|long` calls all returned sourced indexed output under the 30-second client timeout. R5-R6 verified.

## Completion
- Resolved outcomes: R1-R6.
- Commands and artifacts: Focused C11/C20/C33/C39/MCP/config tests; 1,016 library tests in the active worktree; fmt, clippy, JSON contract parsing, dashboard build, model-plugin tests; clean production Docker builds; three live MCP calls; Compose and `/health` checks.
- Constraint and diff-scope check: No dependency, wrapper, session, research surface, model setting, schema, cache, worker, or `/v1/web/fetch` change was added. Unrelated pre-existing dirty-worktree edits were not staged; deployment images were built from clean committed worktrees.
- Final status: complete.
