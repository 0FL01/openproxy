# Goal: Configurable Codex search depth

Status: active
Source: User-approved provider-panel search-depth plan (2026-09-14)
Last updated: 2026-09-14

## Objective
Let the operator select `off`, `low`, `medium`, or `high` for proxy-injected
Codex hosted search from the Codex provider page, with the selected depth
persisted and enforced by the routed request.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and
Primary Evidence. Work on the smallest unresolved outcome. Do not add
requirements from reviews, tests, tools, speculative risks, or optional source
text. Finish when every required outcome is resolved and affected constraints
remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Codex search depth is a persisted provider-level setting.
  - Source: User request for a Codex provider-panel parameter.
  - Acceptance: `/api/settings` reads and PATCHes only
    `off|low|medium|high`; missing state behaves as `medium`, and invalid input
    is rejected without changing persisted state.
  - Primary evidence: Focused settings API tests.
  - Status: verified
  - Evidence: Focused settings API regression persists `low`, returns it, rejects
    `ultra` with 400, and confirms the stored `low` value remains unchanged.

- R2: The selected depth controls proxy-injected hosted search.
  - Source: User-approved semantics after RECON.
  - Acceptance: `off` prevents header-driven injection; `low|medium|high`
    becomes the exact `search_context_size` on the injected Codex tool after
    physical provider/account capability gating.
  - Primary evidence: Focused policy and Codex request transformation tests.
  - Status: verified
  - Evidence: Policy test proves missing=`medium`, all enabled values inject,
    and `off` does not; Codex transform test proves exact `low` wire value while
    preserving a client-native search tool unchanged.

- R3: The Codex provider page exposes the four-value selector.
  - Source: User request for the parameter in the Codex web-panel settings.
  - Acceptance: `/dashboard/providers/codex` loads, displays, and immediately
    persists `Off|Low|Medium|High`, rolls back on failure, and no other provider
    shows the control.
  - Primary evidence: Dashboard build and focused UI-path inspection.
  - Status: verified
  - Evidence: Codex-only guarded selector loads and PATCHes the scalar setting,
    disables during save, and rolls back with notification on error; dashboard
    build passed.

- R4: The verified change is committed, pushed, and builds successfully.
  - Source: User instruction to commit, push, and build.
  - Acceptance: Required Rust/web gates pass and `main == origin/main`; no
    production deployment or live model request is performed.
  - Primary evidence: Rust tests/gates, dashboard build, Git refs.
  - Status: pending
  - Evidence:

### Constraints
- C1: The generic request header remains required; depth does not auto-enable
  search for requests that did not opt in.
- C2: Client-native `web_search` options remain unchanged.
- C3: Non-Codex routes, combos/fusion, response sanitization, citations,
  streaming, cache behavior, and auto-ping remain unchanged.
- C4: Existing settings and credentials remain persisted and secret-safe.

### Non-goals
- `max_tool_calls` or any count limit.
- Line/byte truncation, output-token policy, standalone search, plugin, or MCP.
- Per-account depth, model-list changes, or Claude/OpenCode-specific UI changes.
- Production deployment or live Codex/OpenCode canary.

## Change Envelope
- Target: One validated settings-extra key, one Codex-only provider selector,
  and request-local depth propagation into the existing hosted tool injection.
- Expected paths, symbols, and direct consumers: `src/server/api/mod.rs`,
  `src/server/api/chat.rs`, `src/core/executor/codex.rs`,
  `src/server/api/quota_auto_ping.rs`,
  `web/src/components/providers/ProviderDetailPageClient.tsx`, and nearest tests.
- Allowed and forbidden artifacts: Existing settings.extra/API/React/Serde
  mechanisms only; no dependency, migration, endpoint, persistent schema, or
  new service.
- User or harness budget: Iterative offline implementation, commit/push/build;
  no deployment and no live model call.

## Current Checkpoint
- Closes: R4.
- Smallest next action: Run required Rust/web build gates, inspect the final
  diff, commit and push, then run the requested Docker build without deployment.
- Expected evidence: Gates and build pass, `main == origin/main`, and production
  remains untouched.
- Stop or replan if: A changed-path gate fails or build requires deployment/live
  model access.

## Current State
- Resolved: R1-R3.
- Last relevant evidence: 1,941 library tests, focused settings regression,
  integration compilation, fmt, clippy, and dashboard build pass. Clippy reports
  only six pre-existing unrelated warnings.
- Blocker: None.
- Next: Required gates, commit/push, requested build.

## Material Decisions
- 2026-09-14: Missing setting means `medium` to preserve deployed behavior.
- 2026-09-14: `off` disables header-driven proxy injection; explicit native
  client tools remain client-owned.
- 2026-09-14: Header opt-in remains required at every enabled depth.
- 2026-09-14: User explicitly removed `max_tool_calls` from scope.

## Checkpoint History
- 2026-09-14: Goal created after approved RECON plan; no implementation started.
- 2026-09-14: Validated persisted setting, request-local depth propagation, and
  Codex-only provider selector implemented; focused offline evidence passed.
- 2026-09-14: Commit gates passed: 1,941 library tests, focused settings test,
  integration compilation, fmt, clippy, and dashboard build.

## Completion
- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
