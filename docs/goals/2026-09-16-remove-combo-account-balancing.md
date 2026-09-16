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
  - Primary evidence: Focused Rust tests, dashboard build, OpenCode model-discovery tests, and repository search.
  - Status: pending
  - Evidence:

- R2: Remove proactive provider-account balancing.
  - Source: Approved plan Part B and user requirement that round-robin harms prompt-cache affinity.
  - Acceptance: Eligible accounts are selected deterministically by `(priority.unwrap_or(MAX), id)` without round-robin, sticky, least-loaded, quota, or in-flight-capacity diversion; persisted cooldown/model locks and reactive fallback to the next account remain.
  - Primary evidence: A concurrency regression proving more than ten held requests all use the first account, plus the existing 429 fallback regression.
  - Status: pending
  - Evidence:

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
- Allowed artifacts: Deletions, minimal ownership moves for retained shared helpers/capabilities, focused regression tests, this goal document, and compatibility constants/tombstones required by constraints.
- Forbidden artifacts: New dependencies, migrations that destroy historical rows, replacement routing abstractions, unrelated fallback/proxy redesign, secrets, local configuration, database files, or generated backups.
- User or harness budget: No explicit LOC/time budget. Use one independently buildable commit for Combo removal, one for account-balancing removal, then a final goal/evidence commit if needed.

## Current Checkpoint

- Closes: R1
- Smallest next action: Remove Combo runtime, operational surfaces, and persistence readers/writers while retaining compatibility metadata and inert SQLite rows.
- Expected evidence: Focused Rust tests and dashboard build pass; explicit Combo routes are rejected and model discovery omits Combo.
- Stop or replan if: Removing a surface requires breaking a retained direct-route, alias, v1 compatibility, or persistence-safety constraint.

## Current State

- Resolved: Contract frozen; implementation has not started.
- Last relevant evidence: Recon and three independent audits identified direct consumers and persistence/deployment risks; working tree was clean at `e7b28103`.
- Blocker: None.
- Next: Implement and verify R1.

## Material Decisions

- 2026-09-16: Bare unknown model names retain existing provider inference; only explicit `combo:*` identifiers can be unambiguously rejected after deleting Combo readers.
- 2026-09-16: Keep the `combos` SQLite DDL as an inert rollback-friendly tombstone, but remove every runtime reader and writer.
- 2026-09-16: Keep `openproxy.v1` Combo schema/example as deprecated compatibility metadata and settings strategy fields as fixed ignored response values.
- 2026-09-16: Account selection becomes deterministic only after existing eligibility filters; reactive failure fallback remains unchanged.

## Checkpoint History

- 2026-09-16: Frozen R1-R3 from the approved audited plan. Next: R1 Combo removal.

## Completion

- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
