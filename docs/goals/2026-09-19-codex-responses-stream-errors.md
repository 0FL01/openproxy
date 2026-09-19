# Goal: Codex Responses stream errors

Status: active
Source: User-approved audited implementation plan and instruction to implement, commit, push, and deploy (2026-09-19)
Last updated: 2026-09-19

## Objective
OpenCode receives a valid terminal Responses API error when a Codex stream fails, while OpenProxy no longer injects hosted `web_search` from its legacy OpenCode header/configuration path.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Post-commit `/v1/responses` transport failures emit one valid terminal Responses error event.
  - Source: Reported OpenCode `@ai-sdk/openai` validation failure and approved plan.
  - Acceptance: The event has `type: "error"`, the next `sequence_number`, string `code`, message, and no synthetic completion or retry.
  - Primary evidence: One Codex route regression with a scripted upstream body failure.
  - Status: verified
  - Evidence: Codex route regression emits one `error` at sequence 5 after sequence 4, with string code/message, no completion/DONE, and one upstream request; compat pass-through test also passes.

- R2: The legacy OpenProxy/OpenCode header can no longer inject hosted Codex `web_search`.
  - Source: User instruction to seal web search out of the OpenCode runtime.
  - Acceptance: A header-only request does not add `web_search`; an explicit native client tool remains unchanged.
  - Primary evidence: Focused request/runtime tests.
  - Status: verified
  - Evidence: Focused chat/executor tests prove intent now requires an explicit native tool, no tool is injected, and native search events pass through unchanged.

- R3: OpenCode configuration and dashboard no longer advertise or persist the legacy opt-in.
  - Source: Approved plan.
  - Acceptance: Generated/saved OpenCode config removes the legacy header while preserving unrelated settings, and the dashboard exposes no search toggle/depth control.
  - Primary evidence: Existing settings regression, dashboard build, and OpenCode model plugin checks.
  - Status: verified
  - Evidence: Existing settings regression removes a mixed-case stale header while preserving `X-Keep`; dashboard build and both OpenCode model plugin checks pass.

- R4: The verified change is committed, pushed, deployed, and healthy.
  - Source: User instruction to commit, push, and deploy.
  - Acceptance: The current branch is pushed and the deployed service reports healthy.
  - Primary evidence: Git refs plus deployment and health output.
  - Status: pending
  - Evidence:

### Constraints
- C1: Explicit caller-owned native `web_search` remains provider-native; MCP conversion is deferred.
- C2: No post-commit generation retry or full-stream buffering.
- C3: No dependency, migration, service, public API, or secret/config material is added.
- C4: Unrelated OpenCode headers and model-discovery behavior remain intact.

### Non-goals
- MCP search conversion or tool execution.
- Global removal of Codex native hosted tools.
- New retry, fallback, cache, or CI mechanisms.

## Change Envelope
- Target: Responses error formatting/dispatch, removal of legacy search injection/sanitization, and owned OpenCode configuration/UI/docs.
- Expected paths, symbols, and direct consumers: `src/server/api/chat.rs`, `src/server/api/compat.rs`, `src/core/executor/codex.rs`, `src/server/api/cli_tools.rs`, the Responses translator, OpenCode dashboard components, nearest tests, and stale feature docs.
- Allowed and forbidden artifacts: Existing Rust/React/config mechanisms only; no dependency, migration, service, or MCP implementation.
- User or harness budget: Minimum direct regression evidence; commit, push, deploy, and health-check after verification.

## Current Checkpoint
- Closes: R4 after the closure gates pass.
- Smallest next action: Run the final affected-surface gates and inspect the complete diff.
- Expected evidence: Rust tests, fmt, clippy, dashboard/plugin evidence, and an in-scope diff.
- Stop or replan if: A gate proves an approved outcome or affected contract remains unsatisfied.

## Current State
- Resolved: R1-R3.
- Last relevant evidence: OpenCode settings cleanup, dashboard build, and both model plugin checks pass.
- Blocker: None.
- Next: Implement R2, then R1 and R3.

## Material Decisions
- 2026-09-19: Responses error selection is owned by downstream format, not provider identity.
- 2026-09-19: Use a terminal `error` event, not a fabricated `response.failed` object.
- 2026-09-19: Retire only OpenProxy-owned search injection; preserve explicit native client tools.

## Checkpoint History
- 2026-09-19: Contract frozen from the user-approved audited plan; implementation not yet started.
- 2026-09-19: R2 verified; legacy injection/sanitization removed while explicit native search remains unchanged.
- 2026-09-19: R1 verified; terminal Responses errors are format-owned, sequenced, and do not trigger completion or retry.
- 2026-09-19: R3 verified; OpenCode config/UI/docs no longer advertise the retired header and stale config is cleaned safely.

## Completion
- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
