# Goal: Plan-aware Codex auto-ping

Status: complete
Source: User-approved Codex auto-ping plan (2026-09-14)
Last updated: 2026-09-14

## Objective
After a scheduled or account-level Codex quota reset, send exactly one tiny
`gpt-5.6-luna` request for Plus session windows and weekly-only Pro windows,
with retryable reset state and matching dashboard controls.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and
Primary Evidence. Work on the smallest unresolved outcome. Do not add
requirements from reviews, tests, tools, speculative risks, or optional source
text. Finish when every required outcome is resolved and affected constraints
remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Codex quota polling and target selection are account- and window-aware.
  - Source: Approved plan sections 1 and 2.
  - Acceptance: WHAM receives the connection's ChatGPT account ID; Plus selects
    `session`, weekly-only Pro selects `weekly`, and review quotas are ignored.
  - Primary evidence: Focused Rust quota auto-ping and Codex quota tests.
  - Status: verified
  - Evidence: Account-header, duration classification, and Plus/Pro target tests
    passed; `cargo +1.98.1 check -p openproxy --all-targets` passed.
- R2: Scheduled and repeated mid-window resets produce one retryable ping each.
  - Source: Approved reset detection and pending-state plan.
  - Acceptance: Initial observation does not ping; deadline, forward reset-time
    movement, and explicit usage reset create a deduplicated pending event that
    survives cooldown, failure, and process restart until success.
  - Primary evidence: Deterministic state-transition regression tests.
  - Status: verified
  - Evidence: Scheduled, mid-window, repeated-reset, deduplication, and pending
    serialization tests passed in the focused `quota_auto_ping` suite.
- R3: Auto-ping uses only a tiny `gpt-5.6-luna` request.
  - Source: User requirement and approved plan section 3.
  - Acceptance: Exact Luna lookup with no model fallback, low reasoning, short
    payload, and successful full response drain before marking success.
  - Primary evidence: Focused Rust model/payload tests.
  - Status: verified
  - Evidence: Exact Luna selection and low-reasoning tiny payload regression test
    passed; both response transports now require a successful full drain.
- R4: Dashboard controls describe and expose Plus and weekly-only Pro behavior.
  - Source: Approved plan section 4.
  - Acceptance: Weekly-only Codex connections show the toggle and all auto-ping
    metadata/tooltips reference the next quota window and `gpt-5.6-luna`.
  - Primary evidence: Dashboard build.
  - Status: verified
  - Evidence: Weekly-only toggle condition and copy updated; `pnpm build` passed
    with the repository's existing sourcemap warnings.
- R5: The verified change is committed, pushed, and deployed.
  - Source: User instruction to commit, push, and deploy.
  - Acceptance: `main` equals `origin/main`, production is healthy on the new
    image, and safe production observations confirm the Pro weekly shape and
    Luna availability without an unsolicited ping.
  - Primary evidence: Git refs, container health, and authenticated usage,
    model, and tick output.
  - Status: verified
  - Evidence: Runtime commit `e2d85622` deployed as image
    `sha256:9729cfa7224e`; container and `/health` are healthy. Production reports
    Pro with only `weekly` (10,080 minutes), Luna with `low`, no pending or prior
    ping marker, and the tick made zero attempts because auto-ping is disabled.

### Constraints
- C1: Preserve existing tick response fields; diagnostics are additive.
- C2: Do not consume a Codex reset credit during verification.
- C3: Do not expose credentials or account identifiers in logs or evidence.
- C4: Rebuild `web/dist` after source changes.

### Non-goals
- Database migration, new dependency, new service, or reset-event queue.
- Plan-name or primary/secondary-position heuristics.
- Pricing/model fallback logic.
- Refactoring Claude auto-ping, generic OAuth refresh, or proxy infrastructure.

## Change Envelope
- Target: Codex quota fetch, Codex-only auto-ping decision/persistence/payload,
  existing dashboard toggle visibility and copy, and nearest tests.
- Expected paths, symbols, and direct consumers:
  `src/core/usage/quota_fetcher.rs`, `src/server/api/usage.rs`,
  `src/server/api/quota_auto_ping.rs`, provider limits/connection UI, shared
  auto-ping constants, and this goal document.
- Allowed and forbidden artifacts: Existing connection `extra` may hold one
  event-driven pending marker; no schema, dependency, service, queue, or public
  generic API additions.
- User or harness budget: Minimal diff; use Rust toolchain `+1.98.1` locally.

## Current Checkpoint
- Closes: None; objective complete.
- Smallest next action: Stop.
- Expected evidence: All required outcomes are verified below.
- Stop or replan if: Not applicable.

## Current State
- Resolved: R1-R5 implemented and verified.
- Last relevant evidence: Runtime commit deployed healthy; production Pro
  weekly quota and Luna metadata verified without sending a ping.
- Blocker: None.
- Next: None.

## Material Decisions
- 2026-09-14: Select windows by normalized duration-derived key, never plan name.
- 2026-09-14: Persist only pending events; keep ordinary observations in memory.
- 2026-09-14: Do not add `max_output_tokens`; the current Codex transform drops it.
- 2026-09-14: Production auto-ping is currently disabled; verification did not
  alter user settings merely to manufacture a tick target.

## Checkpoint History
- 2026-09-14: Contract frozen; implementation not started.
- 2026-09-14: R1-R4 implemented; focused Rust tests, all-target check, and web
  build passed; next checkpoint is R5 deployment.
- 2026-09-14: Commit gates passed: 1,931 library tests, fmt, clippy, and web
  build; no warning was introduced by the changed paths.
- 2026-09-14: Runtime commit `e2d85622` pushed and deployed; production health,
  weekly-only Pro quota, Luna availability, and zero ping attempts verified.

## Completion
- Resolved outcomes: R1-R5.
- Commands and artifacts: Focused tests; 1,931-test library suite; fmt; clippy;
  all-target check; dashboard and Docker production builds; authenticated
  production health, usage, model, and tick observations.
- Constraint and diff-scope check: No migration, dependency, service, reset
  credit consumption, credential output, or unrelated provider refactor.
- Final status: complete.
