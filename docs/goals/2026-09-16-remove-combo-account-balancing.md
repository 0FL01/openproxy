# Goal: Remove Combo and proactive account balancing

Status: active
Source: User-approved audited plan on 2026-09-16; implement iteratively, commit, push, and deploy.
Last updated: 2026-09-16

## Objective

Make OpenProxy a deterministic personal proxy router: remove Combo as a runtime and management feature, and always prefer the first eligible provider account by configured priority and stable ID while retaining reactive account fallback.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Commit each independently buildable removal before starting the next. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes

- R1: Remove Combo runtime and management end to end.
  - Source: Approved plan Part A.
  - Acceptance: Combo no longer resolves, executes, appears in model discovery, or has operational API/CLI/dashboard/persistence surfaces; explicit `combo:*` requests fail before upstream dispatch; direct routes and model aliases still resolve.
  - Primary evidence: Existing direct-route/alias tests, dashboard build, OpenCode model-discovery tests, and repository search. Do not retain tests whose sole purpose is proving removed Combo behavior is absent.
  - Status: verified
  - Evidence: Removed Combo execution, resolution, discovery, management, persistence serialization, dashboard, CLI, current docs, literals, and dedicated/deletion-only tests. Retained v1 schema/example and inert SQLite DDL. Focused Rust tests (43), dashboard build (124 pages), clippy/check/fmt, and both OpenCode model-discovery suites passed.

- R2: Remove proactive provider-account balancing.
  - Source: Approved plan Part B and user requirement that round-robin harms prompt-cache affinity.
  - Acceptance: Eligible accounts are selected deterministically by `(priority.unwrap_or(MAX), id)` without round-robin, sticky, least-loaded, quota, or in-flight-capacity diversion; persisted cooldown/model locks and reactive fallback to the next account remain.
  - Primary evidence: A concurrency regression proving more than ten held requests all use the first account, plus the existing 429 fallback regression.
  - Status: verified
  - Evidence: Removed the account registry, strategy types/settings, slot caps, quota tie-breaks, rotation/sticky state, and dashboard controls. Chat, web fetch, and CLI now select by stable `(priority, id)` after their existing eligibility filters. The 11-request concurrency regression and existing reactive fallback test passed; focused account-fallback, settings, database, and backup tests plus dashboard build and clippy passed.

- R3: Deliver the simplification safely.
  - Source: “итеративно реализовать и коммит пуш деплой” and “в цель документируй что делается”.
  - Acceptance: Part A and Part B are independently buildable Conventional Commits, final required gates pass, the goal records current evidence, `main` is pushed to `origin`, production is rebuilt without deleting its persistent volume, and `/health` succeeds.
  - Primary evidence: Git history/push output, final gate output, `docker compose ps`, and `curl -fsS http://127.0.0.1:4623/health`.
  - Status: pending
  - Evidence:

### Constraints

- C1: Preserve direct `provider/model` routing, invocation-only aliases, model capability metadata, provider Available Models, and `ModelSelectModal` consistency.
- C2: Preserve reactive account fallback, explicit priority, credential/model-support filtering, persisted cooldown/model locks, and OAuth refresh behavior.
- C3: Preserve frozen `openproxy.v1.*` compatibility metadata: keep the Combo schema/example and fixed ignored strategy fields in settings responses while removing their operational semantics.
- C4: Keep the SQLite `combos` table/index as an inert tombstone. Runtime import, restore, patch, backup, and export paths must neither expose nor mutate its rows.
- C5: Preserve production secrets, encryption key, and the existing persistent volume; never stage `.env.prod`, runtime data, or backups.

### Non-goals

- Redesigning retryable HTTP error classification, OAuth refresh guards, or concurrent cooldown races.
- Fixing proxy-pool resolution, publishing alias IDs, or adding a special hard 404 for `/dashboard/combos`.
- Converting historical Combo data, dropping the tombstone table, opening a v2 schema namespace, or rewriting historical changelog/goal records.

## Change Envelope

- Target: Combo runtime, management, persistence serialization, dashboard/docs consumers, and proactive account-selection strategy/registry paths.
- Expected paths, symbols, and direct consumers: `src/core/{combo,model,account_fallback}`, chat/web-fetch/model-discovery APIs, CLI command/dispatch/schema/settings surfaces, `AppDb`/`Settings` and SQLite repositories/import/export/patch/backups, directly coupled dashboard components/routes/literals, nearest tests, and current operational docs.
- Allowed artifacts: Deletions, minimal ownership moves for retained shared helpers/capabilities, tests for retained routing behavior, this goal document, and compatibility constants/tombstones required by constraints.
- Forbidden artifacts: New dependencies, migrations that destroy historical rows, replacement routing abstractions, unrelated fallback/proxy redesign, secrets, local configuration, database files, or generated backups.
- User or harness budget: No explicit LOC/time budget. Use one independently buildable commit for Combo removal, one for account-balancing removal, then a final goal/evidence commit if needed.

## Current Checkpoint

- Closes: R3
- Smallest next action: Commit this checkpoint, push `main`, build the production image, take a stopped-volume archive, and recreate the service without deleting its volume.
- Expected evidence: Final checks pass, Git history is clean and pushed, Compose reports the service healthy, and `http://127.0.0.1:4623/health` succeeds.
- Stop or replan if: A gate fails because of the current diff, the production volume or encryption key cannot be preserved, or deployment would require a destructive action.

## Current State

- Resolved: R1 and R2 verified; Combo and proactive account balancing are no longer operational features. Obsolete legacy JS tests and fixtures for removed features were deleted.
- Last relevant evidence: fmt, clippy, dashboard build, both OpenCode model-discovery suites, affected integration tests, and the 1,329 library tests passed. The all-target gate exposed unrelated pre-existing stale tests: `/api/health` exact payload, Kiro URL ordering/import payload assertions, and an intermittent SQLite encryption assertion; no failing test exercises this diff.
- Blocker: None.
- Next: Push, take the raw volume backup during the deployment stop, deploy, verify production, then close the goal.

## Material Decisions

- 2026-09-16: Bare unknown model names retain existing provider inference; only explicit `combo:*` identifiers can be unambiguously rejected after deleting Combo readers.
- 2026-09-16: Keep the `combos` SQLite DDL as an inert rollback-friendly tombstone, but remove every runtime reader and writer.
- 2026-09-16: Keep `openproxy.v1` Combo schema/example as deprecated compatibility metadata and settings strategy fields as fixed ignored response values.
- 2026-09-16: Account selection becomes deterministic only after existing eligibility filters; reactive failure fallback remains unchanged.
- 2026-09-16: Per user instruction, delete tests that only prove removed features are gone, including stale tests left by earlier removal goals; verify retained behavior instead.

## Checkpoint History

- 2026-09-16: Frozen R1-R3 from the approved audited plan. Next: R1 Combo removal.
- 2026-09-16: R1 verified. Combo runtime/management and stale deletion-only tests are removed; direct routes, aliases, frozen schema metadata, and SQLite tombstone remain. Next: commit R1 and implement deterministic account affinity.
- 2026-09-16: R2 verified. Stable priority/ID selection replaces all proactive account balancing while persisted cooldowns and reactive fallback remain. Removed the orphan legacy JS test harness and stale removed-feature fixture fields. Next: commit R2 and run final gates.
- 2026-09-16: Final affected gates passed. The broad suite's unrelated stale failures are documented above; a removed `usage summary` CLI test discovered by the gate was deleted. Production preflight found zero live Combos and created an old-version logical backup. Next: push and deploy with a stopped-volume archive.

## Completion

- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
