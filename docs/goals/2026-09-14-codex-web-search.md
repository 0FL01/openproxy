# Goal: Provider-scoped Codex web search

Status: complete
Source: User-approved audited implementation plan (2026-09-14)
Last updated: 2026-09-19

> Superseded on 2026-09-19: the generic header-driven injection and OpenCode
> controls were retired by `2026-09-19-codex-responses-stream-errors.md`.
> Explicit caller-owned native `web_search` remains supported.

## Objective
Allow agent harnesses to opt into native Codex web search through OpenProxy,
while ensuring non-Codex routes never receive or consume Codex search.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and
Primary Evidence. Work on the smallest unresolved outcome. Do not add
requirements from reviews, tests, tools, speculative risks, or optional source
text. Finish when every required outcome is resolved and affected constraints
remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: A client-independent request contract enables native Codex web search.
  - Source: User request to avoid hardcoded OpenCode detection and support other
    harnesses.
  - Acceptance: A native Responses `web_search` tool is preserved, and the
    generic `X-OpenProxy-Codex-Web-Search: true` opt-in can add the same tool
    without inspecting client identity.
  - Primary evidence: Focused request transformation tests.
  - Status: verified
  - Evidence: Focused unit tests prove header/native intent detection plus
    additive, idempotent native tool handling and `tool_choice: none`.

- R2: Codex search is isolated to a supporting physical Codex route.
  - Source: User requirement that DeepSeek and other models cannot use Codex
    search.
  - Acceptance: Search is enabled only for `provider == "codex"` and the exact
    selected connection/model advertises `search`; non-Codex and unknown or
    unsupported routes receive no injected Codex tool.
  - Primary evidence: Focused routing and account-capability tests.
  - Status: verified
  - Evidence: Exact model/capability tests pass; runtime gating occurs only
    after selecting `provider == "codex"`. A mocked non-Codex route with the
    same header and a misleading `codex/` model prefix stayed on its physical
    provider, bypassed cache, and forwarded no `tools` field.

- R3: OpenCode can opt in without creating a client-specific backend branch.
  - Source: User-approved plan for OpenCode TUI plus reusable harness support.
  - Acceptance: The existing OpenCode settings UI can enable/disable the
    generic header while preserving unrelated configuration; the same header
    remains manually usable by Claude Code and other harnesses.
  - Primary evidence: OpenCode settings tests and dashboard build.
  - Status: verified
  - Evidence: OpenCode settings API regression test proves enable/read/disable
    while preserving an unrelated header; dashboard build passed.

- R4: Search requests do not reuse stale response cache entries or multiply a
  search after upstream search activity begins.
  - Source: User-approved audited plan safeguards.
  - Acceptance: Search-intent requests bypass response cache, and Codex SSE
    search lifecycle output prevents executor retry/fallback classification.
  - Primary evidence: Focused cache decision and Codex SSE tests.
  - Status: verified
  - Evidence: Mocked non-Codex integration test proves header requests bypass
    a populated cache; focused Codex test treats hosted-search lifecycle output
    as user output before transient-error matching.

- R5: The primary OpenCode-shaped path is proven compatible with current
  ChatGPT Codex hosted search.
  - Source: User-approved compatibility spike and rollout plan.
  - Acceptance: One sanitized live `/v1/chat/completions` canary on a catalog-
    supported Codex model produces search activity and a usable final answer;
    negative routes are verified offline.
  - Primary evidence: Sanitized event/status summary plus focused Rust checks.
  - Status: verified
  - Evidence: Pre-change canary returned HTTP 200, nine text chunks, a usable
    234-character final answer containing an HTTP URL, and finish reason
    `stop` for a native hosted-search request.

- R6: The verified change is committed, pushed, deployed, and healthy.
  - Source: User instruction to commit, push, and deploy.
  - Acceptance: `main` equals `origin/main`, the production container is
    healthy on the new runtime image, and a safe production observation
    confirms the feature without an extra negative-route quota call.
  - Primary evidence: Git refs, container health, and sanitized production
    observation.
  - Status: verified
  - Evidence: Runtime commit `c892eabb` is pushed and deployed as image
    `sha256:a59de0740ebb`; container and `/health` are healthy. Production Luna
    remains search-capable, the OpenCode setting remains disabled by default,
    and the deployed dashboard bundle contains the toggle and generic header.

### Constraints
- C1: Existing client tools and `tool_choice` remain unchanged; injection is
  additive and idempotent.
- C2: Auto-ping never enables web search.
- C3: No credentials, prompts, queries, or search-result bodies are logged or
  committed.
- C4: Existing OpenCode configuration outside the owned header is preserved.

### Non-goals
- Standalone `/codex/alpha/search`, proxy-owned tool loops, plugins, or MCP.
- Combo/fusion search semantics in this first slice.
- Structured search progress or full citation fidelity in Chat/Anthropic UIs.
- Automatic per-client backend detection or dedicated Claude Code UI changes.
- Search filters, location controls, or a persistent server policy.

## Change Envelope
- Target: Native hosted Codex `web_search`, generic request intent, physical
  provider/account capability gate, OpenCode opt-in, cache/retry safeguards.
- Expected paths, symbols, and direct consumers:
  `src/server/api/chat.rs`, `src/core/executor/codex.rs`,
  `src/server/codex_catalog.rs`, `src/server/api/quota_auto_ping.rs`,
  `src/server/api/cli_tools.rs`,
  `web/src/components/cli-tools/OpenCodeToolCard.tsx`, and nearest tests.
- Allowed and forbidden artifacts: Existing Rust/React/config mechanisms only;
  no dependency, migration, service, new search endpoint, plugin, or secret.
- User or harness budget: One real Codex canary; all negative checks offline;
  minimal implementation and verification before commit/push/deploy.

## Current Checkpoint
- Closes: None; objective complete.
- Smallest next action: Stop.
- Expected evidence: All required outcomes are verified below.
- Stop or replan if: Not applicable.

## Current State
- Resolved: R1-R6.
- Last relevant evidence: Runtime commit deployed healthy; production catalog,
  default-off setting, and bundled UI/header contract verified without another
  search call.
- Blocker: None.
- Next: None.

## Material Decisions
- 2026-09-14: Request intent, not client identity, owns feature opt-in.
- 2026-09-14: Use native hosted Responses search first; standalone search is a
  separate fallback only after a deterministic native rejection.
- 2026-09-14: Default remains off; OpenCode stores explicit consent as a generic
  provider header.
- 2026-09-14: First slice supports direct Codex models/aliases, not combos or
  fusion.
- 2026-09-14: Codex executor dispatch is owned solely by the resolved physical
  provider; a model-name prefix cannot redirect another provider's credential.

## Checkpoint History
- 2026-09-14: Goal created and contract frozen; no implementation started.
- 2026-09-14: Native hosted-search canary through `/v1/chat/completions`
  succeeded without exposing credentials, query text, or response text.
- 2026-09-14: Generic backend, account capability gate, OpenCode toggle,
  cache bypass, and retry suppression implemented; focused evidence passed.
- 2026-09-14: Commit gates passed: 1,936 library tests, focused integrations,
  fmt, clippy, and dashboard build. Astro check remains unavailable because the
  repository does not install `@astrojs/check`.
- 2026-09-14: Runtime commit `c892eabb` pushed and deployed; healthy image,
  catalog capability, disabled default, and dashboard bundle verified.

## Completion
- Resolved outcomes: R1-R6.
- Commands and artifacts: One sanitized live canary; five focused library
  tests; focused OpenCode and non-Codex integrations; 1,936-test library suite;
  fmt; clippy; dashboard and Docker builds; authenticated production catalog
  and settings observations.
- Constraint and diff-scope check: No dependency, migration, service,
  persistent search policy, standalone endpoint, combo/fusion search behavior,
  extra live negative request, or credential/query/result logging. Existing
  tools, tool choice, auto-ping, and unrelated OpenCode config are preserved.
- Final status: complete.
