# Goal: Codex effort-cache affinity and the working OpenCode V2 plugin

Status: active
Source: user task and audited plan; latest instruction: replace the plugin with the working installed V2 copy, implement the effort-cache fix, commit, push, and deploy.
Last updated: 2026-10-14

## Objective
Ship the installed OpenCode V2 discovery plugin and a stateless Codex Responses affinity fix that preserves client-owned effort history.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Replace the repository discovery plugin with the working installed V2 implementation.
  - Source: latest user instruction; `/root/.config/opencode/plugins/openproxy-models.js`.
  - Acceptance: repository implementation matches that source; tests and install documentation target V2, not V1.
  - Primary evidence: byte comparison and focused Node tests.
  - Status: verified
  - Evidence: exact-copy `cmp` passed; SHA-256 `a39686d39c1f07fc4b7fb51299b1d091b6d27527b0e66103f85275dd0d41e9d7`. `node --test tests/opencode_models*.test.mjs tests/opencode_mask.test.mjs`: 25 passed, no failures/skips. V1-only sidebar wrapper/tests retired; independent mask unchanged.
- R2: Preserve Codex cache affinity and chronological effort updates.
  - Source: original A1/A2 and the corrected audit plan.
  - Acceptance: ordinary Responses sends canonical `session-id` from the final representable cache key, never account identity; high/low/high preserves initial top-level high, client input order and key. Keep ordinary non-marker behavior; do not silently drop conflicting non-default mode/truncation/context-management values in marked requests.
  - Primary evidence: baseline red/green executor mock and one three-call full-router mock, plus focused transformer cases.
  - Status: verified
  - Evidence: both baseline failures are now green. All 24 executor tests, four focused effort tests and 13 full-router C20 tests passed. The three-call route preserves top-level high, the same body key/header and exact chronological input prefixes; existing private diagnostics contain stable nonempty account/key/envelope HMACs and the fabricated upstream usage, without raw fixture secrets.
- R3: Verify the changed paths and existing passive diagnostics without paid calls.
  - Source: original A0/A4/A5.
  - Acceptance: required Rust gates and applicable Node tests pass; captured upstream JSON/headers and existing diagnostic HMAC/usage fields agree. No claim of a physical cache hit from mocks.
  - Primary evidence: recorded commands and test results.
  - Status: verified
  - Evidence: all commands below passed. The first parallel C20 run passed 12/13 and failed an existing TRACE-capture assertion with an empty buffer; serial C20 passed 13/13 without a runtime/test change. The earlier mock URL assertion was corrected before the final executor gate. Exact OpenCode 2.0.29 binary execution and physical upstream cache reuse remain not verified; no paid generation calls were made.
- R4: Commit, push to origin/main, and deploy with the existing Compose configuration.
  - Source: latest explicit user authorization.
  - Acceptance: intended commits are on origin/main; rebuilt production container is healthy with the intended runtime revision; existing persistent data and environment retained.
  - Primary evidence: Git remote SHA, Compose build/up, health and build metadata.
  - Status: pending
  - Evidence: origin is `git@github.com:0FL01/openproxy.git`; the old production image is preserved as `openproxy:rollback-2b45f570` (`sha256:9a81805c983588f5f2d2732a775ed9254745f7f8efcaa57c6699809608701881`).

### Constraints
- No proxy history/session/cache manager, synthesized updates, effort locking, account stickiness, auth/fallback/retry changes, or new telemetry schema.
- Preserve client cache keys and existing protocol normalization; no prompt/credential/session identifiers in persisted diagnostics.
- Keep production `.env.prod`, encryption keys and `openproxy-prod-data`; no volume removal or database migration.
- No paid/live generation calls. Use loopback upstream mocks and non-generation deployment health checks.

### Non-goals
- OpenCode V1 support is explicitly superseded by the latest instruction.
- No V2 lifecycle framework, temporary provider seeds, hot-reload guarantees, canonical-alias support, sidebar or Claude-mask migration.
- Do not modify the installed working client copy or external client configuration.

## Change Envelope
- Runtime: `src/core/executor/codex.rs`; `plugins/openproxy-models.js` replaced from the approved installed source.
- Evidence: nearest Codex executor/C20/unit tests and OpenCode discovery tests; retire V1-only tests/consumers made obsolete by the authorized replacement.
- Documentation: this goal and relevant plugin installation instructions.
- No new dependency, service, schema, persistent state, or unrelated refactor.

## Implementation Plan
1. Replace the V2 plugin and update its nearest tests/docs. Independently capture the Codex baseline on a loopback upstream, first moving the four live-URL tests to mocks.
2. Apply the small canonical-affinity fix after final body/size validation. Preserve correct input markers; retain old omission for standard/null/empty option forms while forwarding non-default conflict-bearing fields only in marked requests. Prove the three-call route and existing diagnostic fingerprints.
3. Run the required gates, inspect and commit intended changes, push origin/main, build/recreate the existing Compose service and verify health/revision. Record completion; a documentation-only completion commit may follow the deployed runtime commit.

## Current Checkpoint
- Closes: R4.
- Smallest next action: create the three reviewed atomic commits, push origin/main, then build/recreate the existing Compose service with the committed runtime SHA.
- Expected evidence: remote SHA, healthy new container, compiled revision and unchanged named volume; completion record.
- Stop or replan if: push is rejected or the existing deployment cannot be rebuilt/verified without changing secrets or persistent data.

## Current State
- Resolved: R1, R2 and R3; scoped runtime/test diff reviewed.
- Last relevant evidence: 25 Node, six cache, 24 executor, 13 serial C20 and four effort tests passed; formatting, all-target clippy and diff whitespace checks passed. Production remains on the original container and named volume, with its image tagged for rollback.
- Blocker: none under the user's approved plugin replacement.
- Next: atomic commits, push and Compose deployment.

## Verification Commands
All passed on the final implementation:
```sh
node --test tests/opencode_models*.test.mjs tests/opencode_mask.test.mjs
cargo fmt --all --check
cargo test --locked --lib codex_cache
cargo test --locked --test codex_executor
cargo test --locked --test lean_proxy_codex_c20 -- --test-threads=1
cargo test --locked --lib codex_effort
cargo clippy --locked --all-targets
git diff --check
```

## Material Decisions
- 2026-10-14: user explicitly selected the already-working V2 implementation instead of V1 parity/lifecycle redesign, and authorized push/deployment.
- 2026-10-14: existing server invariants remain; plugin behavior is the approved installed implementation, not a new compatibility promise.

## Checkpoint History
- 2026-10-14: frozen narrowed scope before implementation; clean baseline verified.
- 2026-10-14: R1 exact V2 replacement verified. First Rust baseline command exhausted its 120-second compilation budget before running tests; retried with no timeout, without a runtime change.
- 2026-10-14: A0 canonical-header baseline failed (`None` vs the cache-key bytes). The unsupported-combination baseline separately failed (`reasoning.mode: null` vs `pro`), after returning the original upstream 400 with one attempt. Both fixes remain Codex-local and stateless.
- 2026-10-14: final executor/cache gates passed. Parallel C20 run passed 12/13, including both new regressions; serial execution is the next bounded check for the existing TRACE-capture failure. No TRACE/runtime change was made.
- 2026-10-14: serial C20 passed 13/13, focused effort tests 4/4, all-target clippy and whitespace checks passed. R1–R3 verified; old production image retained for rollback. Next: commit, push, deploy.

## Completion
- Pending implementation, checks, push and deployment.
