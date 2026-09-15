# Goal: OpenCode discovered model capabilities

Status: complete
Source: user instructions in the current task
Last updated: 2026-09-15

## Objective
OpenCode 1.18.31 receives accurate discovered limits, attachment/modalities, and
reasoning effort choices for Codex and OpenCode Go/Zen models from OpenProxy,
then the verified change is committed, pushed, and deployed.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and
Primary Evidence. Work on the smallest unresolved outcome. Do not add
requirements from reviews, tests, tools, speculative risks, or optional source
text. Finish when every required outcome is resolved and affected constraints
remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Preserve authoritative OpenCode Go/Zen model facts from models.dev.
  - Source: user requires context, vision, and reasoning effort metadata for OpenCode Go/Zen.
  - Acceptance: `/v1/models` carries exact context/input/output, attachment/modalities, and exact effort choices supplied by models.dev.
  - Primary evidence: targeted Rust parser and `/v1/models` tests.
  - Status: verified
  - Evidence: `cargo test -p openproxy --lib models_ -- --nocapture` passed; parser and API assertions cover input/output/context, attachment, exact modalities, and exact/absent effort metadata.
- R2: Complete Codex discovery metadata without fabricated defaults.
  - Source: user requires the same behavior for Codex through one logic path.
  - Acceptance: live Codex context/modalities/efforts win; a matching known capability may fill only missing output, while unknown models receive no default limit.
  - Primary evidence: targeted capability resolver and `/v1/models` tests.
  - Status: verified
  - Evidence: `known_output_lookup_never_returns_floor_defaults` and `codex_dynamic_llm_and_static_image_are_kind_aware` passed; known GPT output is filled while an unknown Codex model has no output default.
- R3: Resolve the metadata into valid OpenCode 1.18.31 models.
  - Source: user requires automatic TUI discovery without hacks.
  - Acceptance: the plugin preserves complete limits and vision/attachment, exposes only authoritative effort variants, disables unsupported built-ins, and preserves explicit local overrides.
  - Primary evidence: Node plugin test and real `opencode models --verbose` smoke test.
  - Status: verified
  - Evidence: Node plugin tests and the isolated OpenCode 1.18.31 CLI smoke passed, including resolved attachment, image input, complete limit, and only the authoritative `high` variant.
- R4: Publish and deploy the verified implementation.
  - Source: “потом коммит пуш деплой”.
  - Acceptance: one atomic commit is on `origin/main`; production is rebuilt/restarted and passes health plus authenticated model-metadata observation when credentials are available locally.
  - Primary evidence: git remote state, Compose health, and runtime `/v1/models` response.
  - Status: verified
  - Evidence: feature commit `b4f08404` is on `origin/main`; `docker compose up -d --build` rebuilt and restarted production; Compose reports healthy; authenticated local `/v1/models` and root OpenCode 1.18.31 verbose output show exact Codex and `ocg` metadata.

### Constraints
- C1: GLM provider behavior is outside this scope.
- C2: No model-name heuristics may invent limits, modalities, or effort levels.
- C3: Codex and models.dev must feed the existing shared OpenCode metadata path.
- C4: Do not use subagents.
- C5: Keep credentials in provider options/environment and out of logs and git.

### Non-goals
- Changing OpenCode itself or its 32k per-step output cap.
- Adding capability data for providers other than Codex and OpenCode Go/Zen.
- Reworking the provider/combo capability catalog beyond the known-only lookup needed by Codex.

## Change Envelope
- Target: model-source parsing, known capability lookup, `/v1/models` OpenCode metadata, discovery plugin, and nearest tests.
- Expected paths, symbols, and direct consumers: `src/core/model/models_dev.rs`; `src/core/combo/capabilities.rs`; `src/server/codex_catalog.rs`; `src/server/api/models_metadata.rs`; `src/server/api/v1_models.rs`; `plugins/openproxy-models.js`; their direct tests and root-installed plugin copy.
- Allowed and forbidden artifacts: additive metadata and tests are allowed; no dependency, schema migration, persistent state, service, or generated user config.
- User or harness budget: minimal diff, no subagents; commit, push, and production deploy after verification.

## Current Checkpoint
- Closes: none; closure check passed.
- Smallest next action: stop.
- Expected evidence: complete.
- Stop or replan if: not applicable.

## Current State
- Resolved: R1-R4.
- Last relevant evidence: production OpenProxy and root OpenCode 1.18.31 expose the deployed source metadata correctly.
- Blocker: none.
- Next: none.

## Material Decisions
- 2026-09-15: GLM is explicitly excluded; Codex and OpenCode Go/Zen are the only provider sources in scope.
- 2026-09-15: generic capability defaults are not model facts; only a matched table entry's explicit `maxOutput` may complete a Codex limit.

## Checkpoint History
- 2026-09-15: contract frozen after RECON; implementation started with R1/R2.
- 2026-09-15: R1/R2 verified through source-to-API tests; R3 verified through plugin and real OpenCode 1.18.31 resolution.
- 2026-09-15: `b4f08404` pushed and deployed; health, authenticated API metadata, and real OpenCode resolution verified R4.

## Completion
- Resolved outcomes: R1-R4 verified.
- Commands and artifacts: targeted Rust tests; Node tests; OpenCode 1.18.31 CLI smoke; `cargo fmt --all -- --check`; Clippy all targets/features; 1728 lib tests; 7 custom-model API tests; root plugin byte match; Compose build/health; authenticated runtime `/v1/models`; live `opencode models ludka2 --refresh --verbose`.
- Constraint and diff-scope check: only Codex/OpenCode Go/Zen discovery paths, shared known capability lookup, plugin, tests, and this goal changed; no dependency, persistence, secret, user config, or GLM-specific logic added.
- Final status: complete.
