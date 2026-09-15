# Goal: API-key application logs

Status: active
Source: User request to provide API-key request logs comparable to OmniRoute `Logs / Application logs`, without WebSocket, followed by the instruction to preserve the plan and implement, commit, push, and deploy it end to end.
Last updated: 2026-09-15

## Objective

Provide a durable `/dashboard/logs` request-attempt journal attributed to API-key IDs and names, with server-side filtering and pagination and browser-safe HTTP polling, then commit, push, and deploy the verified result to the repository's production Docker Compose service.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes

- R1: Persist structured application-log records for authenticated inference requests.
  - Source: “хочу видеть логи для апи ключей как у Omnirouter `Logs Application logs`”.
  - Acceptance: Each provider attempt has a durable record containing request/attempt identity, shared correlation identity, API-key ID/name snapshot, endpoint, requested/actual model, provider connection, outcome/status, duration, token counts, and bounded error summary. Retry, fallback, and combo/fusion attempts share a correlation ID. Streaming attempts are not marked complete before their body finishes.
  - Primary evidence: Targeted repository/runtime tests covering successful, failed/fallback, and streaming attempt persistence.
  - Status: verified
  - Evidence: `cargo test -p openproxy --test chat_completions` passed 13 tests; the streaming test verifies completion after body drain and the account-fallback test verifies error/success rows sharing one correlation ID. `cargo test -p openproxy --lib request_repo` and migration tests passed.

- R2: Expose application logs through the authenticated dashboard API without raw API-key values.
  - Source: API-key log requirement and the agreed plan.
  - Acceptance: The dashboard-authorized API reads the durable log store with server-side page/page-size and API-key/status/provider/model/date/correlation filters; responses contain API-key ID/name but no raw key.
  - Primary evidence: Targeted API and SQLite tests for filtering, pagination, persistence, and key redaction.
  - Status: verified
  - Evidence: `cargo test -p openproxy --test api_usage_routes` passed and verifies combined provider/API-key/status filtering, pagination, key ID/name output, and absence of the presented raw key. SQL repository filter/pagination tests passed.

- R3: Provide an OmniRoute-like `/dashboard/logs` page.
  - Source: User reference to `https://ludka.bash8.de/dashboard/logs` and request for the proposed examples.
  - Acceptance: The dashboard contains a discoverable Application Logs page showing status, actual/requested model, provider/account, API-key name, tokens, duration, timestamp, filters, pagination, manual refresh, and an on-demand metadata/error detail view.
  - Primary evidence: Successful Astro check/build and a runtime HTTP/UI smoke against the embedded dashboard.
  - Status: verified
  - Evidence: `pnpm run build` generated `/dashboard/logs.html`; local embedded runtime returned HTTP 200 for `/dashboard/logs` and an authenticated HTTP 200 for `/api/usage/request-details`.

- R4: Use browser-safe HTTP polling instead of a persistent socket.
  - Source: “желательно обойтись без вебсокета”.
  - Acceptance: The logs page uses ordinary HTTP fetch polling, does not use WebSocket/EventSource, avoids overlapping requests, pauses while hidden or away from page 1, and refreshes on focus/manual action.
  - Primary evidence: Source inspection plus successful dashboard build and runtime API smoke.
  - Status: verified
  - Evidence: The focused logs client contains no WebSocket/EventSource usage and uses chained 10-second fetch polling with abort-on-cleanup, visibility pause, focus/manual refresh, and page-1 gating. Dashboard build passed.

- R5: Deliver the completed feature.
  - Source: “потом коммит пуш деплой”.
  - Acceptance: The verified focused diff is committed on `main`, pushed to `origin/main`, deployed through the repository production Docker Compose service, and the production health endpoint responds successfully.
  - Primary evidence: Git commit/push output, `docker compose up -d --build`, `docker compose ps`, and `curl -fsS http://127.0.0.1:4623/health`.
  - Status: in_progress
  - Evidence:

### Constraints

- C1: No WebSocket or EventSource/SSE transport for the logs dashboard.
- C2: Never persist or return a raw client API-key value in the new application-log path.
- C3: Preserve the existing `openproxy.v1.*` additive-only schema contract.
- C4: Preserve SQLite WAL persistence and the production `openproxy-prod-data` volume during deployment.
- C5: Dashboard changes must be rebuilt into `web/dist` before the Rust binary/deployment is considered verified.

### Non-goals

- Capturing or displaying complete prompt, response, translated payload, or raw stream chunks.
- Adding JSON artifact files, a log export feature, configurable table columns, or a new retention service.
- Replacing usage accounting or correcting unrelated existing raw-key exposure in legacy usage APIs.
- Exposing per-key logs directly to inference-key callers; this is an administrator dashboard surface.
- Reworking process console logs at `/dashboard/console-log`.

## Agreed Implementation Plan

1. Keep `usageHistory` as accounting and make the existing SQLite `requestDetails` table the source of truth for application logs.
2. Extend `requestDetails` compatibly with API-key ID/name and correlation columns plus query indexes; retain bounded summary metadata in its existing JSON data column and never store the raw key.
3. Retain the authenticated `ApiKey` object in the chat path, create one correlation ID per incoming request, create one row per provider attempt, and finalize success/error/interrupted state at the actual non-stream or stream lifecycle boundary.
4. Extend `request_repo.rs` with attempt start/finish, SQL list/count filters, and detail lookup. Make `/api/usage/request-details` query SQLite directly and extend its response additively.
5. Add `/dashboard/logs`, a sidebar entry, filters, server-side pagination, manual refresh, and a metadata/error drawer. Remove the obsolete hidden string-splitting logger after the replacement is wired.
6. Poll with ordinary fetch at a conservative interval. Pause in hidden tabs and off the first page, refresh on focus, abort stale fetches, and prevent overlapping requests.
7. Reuse the existing 30-day `requestDetails` cleanup. On startup, mark stale pending records interrupted rather than adding another retention mechanism.
8. Verify the narrow DB/API/runtime paths, then run the repository commit gates and dashboard build, inspect the final diff, commit, push, deploy with production Compose, and check health/UI/API.

## Change Envelope

- Target: Authenticated chat/messages/responses routing, SQLite request-detail persistence/querying, dashboard usage API, and the dashboard logs surface.
- Expected paths, symbols, and direct consumers:
  - `src/db/sqlite/schema.rs`, `migrations.rs`, `repo/request_repo.rs`
  - `src/server/api/chat.rs`, compatibility handlers that delegate to it, `src/server/api/usage.rs`, `src/server/api/mod.rs`
  - startup cleanup/recovery in `src/main.rs` only if needed for stale pending rows
  - nearest DB/API/chat tests
  - `web/src/pages/dashboard/logs/`, sidebar, and a focused logs client/detail component
  - obsolete `web/src/shared/components/RequestLogger.tsx` and its direct export/consumer only when replacement is connected
- Allowed artifacts: Rust/TypeScript/Astro implementation, SQLite additive migration/indexes, focused tests, this goal document, regenerated `web/dist` only if tracked by repository convention.
- Forbidden artifacts: New dependencies, services, workers, sockets, payload artifact files, secrets, local config, production database files, or unrelated refactors.
- User or harness budget: No explicit time/LOC budget; use the smallest complete implementation.

## Current Checkpoint

- Closes: R5
- Smallest next action: Stage the reviewed focused diff, commit and push it, deploy production Compose without removing its volume, and verify production health and logs page.
- Expected evidence: Commit/push output, healthy Compose service, HTTP 200 health/logs page.
- Stop or replan if: The production environment lacks its existing `.env.prod`/volume or the deployment health check fails.

## Current State

- Resolved: R1-R4. Durable attempt persistence, API-key attribution/correlation, streaming lifecycle finalization, SQL-backed API filtering/pagination, and the HTTP-polling dashboard page are implemented and verified.
- Last relevant evidence: `cargo fmt --check`, `cargo clippy --all-targets --all-features`, all 1,736 lib tests, targeted API/chat tests, dashboard build, and local runtime smoke passed.
- Blocker: None.
- Next: Commit, push, and deploy R5.

## Material Decisions

- 2026-09-15: Interpret OmniRoute “Application logs” as request/provider-attempt logs, not process console output.
- 2026-09-15: Use SQLite `requestDetails` rather than a new table or usage history.
- 2026-09-15: Store metadata/error summaries only; full prompt/response payload capture is excluded.
- 2026-09-15: Use ordinary HTTP polling; no WebSocket, EventSource, or dashboard SSE.
- 2026-09-15: Use one row per provider attempt with correlation grouping because fallback visibility is central to router logs.

## Checkpoint History

- 2026-09-15: RECON complete; local OmniRoute source confirmed list polling at 10 seconds and separate attempt rows with API-key snapshots/correlation IDs. Contract frozen; next checkpoint is SQLite persistence.
- 2026-09-15: R1/R2 implementation checkpoint passed targeted migration, repository, API, streaming, and fallback tests. R3/R4 dashboard build passed and generated `/dashboard/logs.html`; next checkpoint is closure verification.
- 2026-09-15: R1-R4 closure evidence passed: fmt, clippy, 1,736 lib tests, targeted integrations, dashboard build, and local runtime HTTP smoke. R5 delivery remains.

## Completion

- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
