# Goal: Codex client version and shared headers

Status: active
Source: User request to audit the plan, require shared provider/search headers, then create a goal, implement iteratively, commit, push, and deploy (2026-09-25). Production Compose on this host and `origin/main` confirmed by user.
Last updated: 2026-09-25

## Objective
OpenProxy advertises Codex CLI 0.157.0 consistently to Responses, model discovery, and standalone web search through one shared header path; the verified change is committed, pushed to `origin/main`, and deployed to the production Compose service.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Stop substantive work at a proven external blocker, an approved budget boundary, or when no remaining in-scope action has a falsifiable expected result; record the exact evidence and smallest unlock.

## Frozen Contract

### Required Outcomes
- R1: Advertise the selected stable Codex CLI version consistently.
  - Source: User request to raise the Codex CLI version and approved audited plan (2026-09-25).
  - Acceptance: Responses, model discovery (`client_version` query and minimum-version filter), and standalone search use `0.157.0`, `codex_cli_rs`, and matching User-Agent; no search-only outdated version remains.
  - Primary evidence: Focused version/catalog test and captured outgoing requests.
  - Status: verified
  - Evidence: `CODEX_CLIENT_VERSION` and UA are pinned to 0.157.0; `lean_proxy_codex_c20` captures `Version`, `originator`, and UA on Responses, catalog, and search. Catalog uses the same constant in its `client_version` query and filter; `parser_filters_and_sanitizes_codex_models` passes.
- R2: Share Codex identity/account header logic without duplicating it across these outbound requests.
  - Source: User requirement that Codex web search headers have parity with the provider and use unified logic without duplication.
  - Acceptance: Search emits shared `Authorization`, `Content-Type`, `originator`, `Version`, `User-Agent`, and canonical nonempty `ChatGPT-Account-ID` where present, as do Responses and the catalog; endpoint-specific `Accept`, session identity, URLs, bodies, and private credentials remain owned by their callers.
  - Primary evidence: One mock capture of the affected upstream requests, including account choice and search without a copied Responses session header.
  - Status: verified
  - Evidence: One internal `build_codex_headers()` supplies all three paths; C20 verifies canonical account choice with conflicting `workspaceId`, per-account bearer on fallback, endpoint-specific Accept, and no search `session_id`. `test_codex_headers_advertise_current_client` passes.
- R3: Commit, push, and deploy the verified change.
  - Source: User instruction “формируй goal с копией плана и итеративно реализовать коммит пуш деплой” and confirmation of production Compose and origin/main.
  - Acceptance: Only goal-owned code/tests/docs are committed, `origin/main` matches the commit, production Compose `openproxy-prod` is running the new image with its existing volume and health check passes on `127.0.0.1:4623`.
  - Primary evidence: Staged diff, git refs, Compose service status, and `/health`.
  - Status: pending
  - Evidence:

### Constraints
- Preserve authenticated, stateless one-shot `/v1/mcp` search and account fallback; do not turn search `body.id` into a session header.
- Do not fabricate CLI turn/session metadata or claim complete Codex CLI wire parity; reuse only common identity/account headers.
- Keep provider credentials private, maintain API-key/auth error behavior, and preserve production persistent volume.

### Non-goals
- Dynamic CLI version fetching, full CLI session/turn/header emulation, model-catalog redesign, frontend build, unrelated provider or OAuth changes, live credential-bearing tests.

## Change Envelope
- Target: Version constants and shared outbound Codex headers for Responses, models catalog, and standalone search.
- Expected paths and direct consumers: `src/core/config/app_constants.rs`, `src/core/executor/codex.rs`, `src/core/executor/codex_search.rs`, `src/core/executor/mod.rs`, `src/server/codex_catalog.rs`, existing `src/core/usage/quota_fetcher.rs::codex_account_id`, and closest tests (`src/core/executor/codex.rs`, `src/server/codex_catalog.rs`, `tests/lean_proxy_codex_c20.rs`), plus this goal.
- Allowed and forbidden artifacts: At most one small internal shared header helper in the existing Codex executor area; no new dependency, public generic API, schema, persistent state, service, or unrelated formatting.
- Deployment: One scoped commit pushed to `origin/main`, then `docker compose up -d --build` for `openproxy-prod` and health verification; no volume deletion or unrelated services.

## Approved Implementation Plan (copy of audited plan)
1. Pin `0.157.0` in `CODEX_CLIENT_VERSION` and `CODEX_USER_AGENT`. The catalog already uses that constant for `client_version` and `minimal_client_version`; do not redesign its filter.
2. Build common Codex headers once for Responses, standalone search, and catalog: `Authorization`, `Content-Type`, `originator`, `Version`, `User-Agent`, and canonical nonempty `ChatGPT-Account-ID` (reuse `codex_account_id()`). Remove the separate search User-Agent. Keep `Accept` and session headers request-specific; do not turn search `body.id` into `session_id`.
3. Check outgoing requests against a local mock upstream: version/identity parity, account choice when both account fields exist, search without Responses session header, and relevant `Accept`; reuse nearest tests and add one search header regression if absent. Run targeted Rust checks, inspect diff, commit, push, production Compose deploy, and verify health.

## Current Checkpoint
- Closes: R3.
- Smallest next action: Review only the intended diff and secret hygiene, commit goal-owned paths (the new goal is ignored by default and must be staged explicitly), push `origin/main`, deploy production Compose preserving its volume, and verify health.
- Expected evidence: Git refs match, Compose reports the updated image healthy, and `/health` succeeds.
- Stop or replan if: Push is rejected, Compose fails, or health fails; record the exact blocker without touching persistent data.

## Current State
- Resolved: R1/R2. Production deploy target confirmed.
- Last relevant evidence: `cargo test -p openproxy --test lean_proxy_codex_c20` (9 passed), `cargo test -p openproxy --lib test_codex_headers_advertise_current_client`, `cargo test -p openproxy --lib parser_filters_and_sanitizes_codex_models`, `cargo fmt --all --check`, and `cargo clippy -p openproxy --all-targets` succeeded. Strict `clippy -- -D warnings` fails on three existing unrelated warnings in `proxy_pool_ops.rs`, `codex_search.rs`, and `request_logger.rs`; none is in this diff.
- Blocker: None.
- Next: Commit, push, deploy, and verify R3.

## Material Decisions
- 2026-09-25: “Parity” means shared declared client identity and correct account, not byte-for-byte transport/session parity with the native CLI.
- 2026-09-25: User confirmed `origin/main` and production Compose `openproxy-prod` on this host, preserving the volume.

## Checkpoint History
- 2026-09-25: Contract frozen from the audited plan; implementation not yet started.
- 2026-09-25: R1/R2 verified via captured requests and focused tests. Strict Clippy found three pre-existing warnings; non-strict Clippy and formatting pass. Next: R3.

## Completion
- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
