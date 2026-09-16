# Goal: Migrate the dashboard to Leptos

Status: complete
Source: user-approved RECON plan and audit, 2026-09-16
Last updated: 2026-09-16

## Objective

Replace the Astro/React/TypeScript dashboard with a Rust Leptos CSR dashboard,
ship it in the existing single OpenProxy binary without Node in the main build,
then commit, push, deploy, and verify the production service.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and
Primary Evidence. Work on the smallest unresolved outcome. Do not add
requirements from reviews, tests, tools, speculative risks, or optional source
text. Finish when every required outcome is resolved and affected constraints
remain satisfied.

## Frozen Contract

### Required Outcomes

- R1: Use Rust 1.98 for the repository build toolchain.
  - Source: User: "раст версию поднимем с 1.85 до 1.98".
  - Acceptance: Cargo metadata, pinned toolchain, Docker, and CI build with Rust 1.98.
  - Primary evidence: `rustc --version`; `cargo check -p openproxy` under the pinned toolchain.
  - Status: verified
  - Evidence: `rust-toolchain.toml` pins 1.98.1; Cargo metadata, CI, release, and Docker use Rust 1.98; local backend check and production Docker build succeeded.

- R2: Replace the dashboard implementation with Leptos CSR built by Trunk.
  - Source: Approved corrected migration plan.
  - Acceptance: Every current dashboard route and required user flow is available from the Leptos build; Astro/React/TypeScript are not used by the dashboard.
  - Primary evidence: clean Trunk release build plus route/action smoke against `--web-dir`.
  - Status: verified
  - Evidence: `dashboard` is a browser-only Leptos workspace crate; all prior canonical routes are registered; `trunk build --release` and the Docker build succeeded; embedded dynamic deep-link, WASM MIME, and missing-asset 404 smokes passed. Astro/React/TypeScript source and build manifests were removed.

- R3: Preserve the core provider-to-OpenCode workflow with one model inventory implementation.
  - Source: Approved plan and AGENTS.md core product surfaces.
  - Acceptance: Provider Available Models and the model picker derive the same catalog/live/custom/alias/disabled/free-only inventory, and the selected model can be saved to OpenCode.
  - Primary evidence: model inventory fixtures and one end-to-end provider → picker → OpenCode check.
  - Status: verified
  - Evidence: `model_inventory.rs` has five focused passing fixtures; provider detail and the shared picker both call `ModelState::inventory`; OpenCode uses that picker. A disposable-container smoke persisted a provider, custom/disabled/free-only model state, applied OpenCode settings, and read the state back.

- R4: Replace Monaco with a Rust-rendered textarea while preserving Translator load, edit, save, and translate behavior.
  - Source: User-selected option: "заменить Monaco на Rust-rendered textarea, сохранив load/edit/save/translate".
  - Acceptance: Translator performs those four operations without Monaco or a JavaScript editor adapter.
  - Primary evidence: Translator route smoke covering load/edit/save/translate.
  - Status: verified
  - Evidence: `dashboard/src/pages/translator.rs` uses a Rust `<textarea>` and implements load, edit, save, formatting, and the three-step translate pipeline; Monaco and the Node dashboard dependencies were removed; dashboard check and Trunk release build pass.

- R5: Remove Node from the main OpenProxy build and keep single-binary deployment.
  - Source: Approved corrected migration plan.
  - Acceptance: Docker, CI, release, installer, updater, and local main build produce the embedded dashboard without Node/pnpm; the supported OpenCode JavaScript plugin may remain.
  - Primary evidence: clean Docker build with no Node stage and a running image serving the embedded dashboard.
  - Status: verified
  - Evidence: Docker, CI, release, installer, updater, and dev scripts build the dashboard with Rust/Trunk before the native binary. `docker compose build` produced image `sha256:d9975b995ae60c4c68f118926149c351ef409bf5b83f13b13cbbd2aa070ab3b4`; runtime inspection found no node/npm/pnpm and embedded dashboard smoke passed.

- R6: Commit, push, and deploy the completed migration.
  - Source: User: "а потом коммит пуш деплой".
  - Acceptance: Verified changes are committed on the current branch, pushed to `origin`, deployed with the repository production Compose configuration, and `/health` succeeds.
  - Primary evidence: pushed commit SHA, `docker compose ps`, and successful `curl http://127.0.0.1:4623/health`.
  - Status: verified
  - Evidence: commit `0f4f6569` was pushed to `origin/main`; production Compose recreated `openproxy-prod-openproxy-1` from `openproxy:prod`; `/health`, Leptos deep link, WASM MIME, and missing-asset 404 checks passed and Compose reports healthy.

### Constraints

- C1: Keep the existing Axum HTTP/JSON API as the browser/server boundary; do not add Leptos SSR, hydration, or server functions.
- C2: Use one production dashboard, not route-level Astro/Leptos coexistence or an embedded legacy fallback.
- C3: Preserve persisted provider configuration, auth/security behavior, local browser storage formats, streaming behavior, and dynamic deep links.
- C4: Keep `plugins/openproxy-models.js`; Node-free main build does not mean a JavaScript-free repository.
- C5: Production downtime is allowed. Rollback uses the previous binary/image, not a second embedded frontend.
- C6: Do not combine the dashboard cutover with a database schema migration or backend API redesign.

### Non-goals

- SSR, hydration, Leptos server functions, route-level micro-frontends, or a generated API client.
- A new generic state framework, repository layer, event bus, or streaming abstraction.
- Rewriting the supported OpenCode plugin solely to remove JavaScript from the repository.
- Pixel-perfect screenshot parity or redesigning the dashboard.

## Change Envelope

- Target: dashboard source/build/serving, directly consumed API DTOs, build and release automation, and deployment documentation.
- Expected paths, symbols, and direct consumers: `dashboard/`, workspace manifests and lockfile, `build.rs`, `src/server/dashboard/`, dashboard version reporting, `Dockerfile`, CI/release workflows, source install/update/dev scripts, and Astro-specific documentation.
- Allowed artifacts: one browser-only Rust crate, Leptos/Trunk/WASM dependencies, static CSS/assets, targeted fixtures and browser/runtime smoke checks.
- Forbidden artifacts: new persistent stores, database migrations, alternate services, dual production frontend, committed secrets or local configuration.
- User or harness budget: iterative implementation is allowed; production downtime is allowed.

## Execution Plan

1. Record the route/action inventory and classify model-contract discrepancies.
2. Pin Rust 1.98 and establish a non-throwaway Leptos CSR/Trunk foundation.
3. Implement the core path in small checkpoints: catalog/model engine → provider model controls → picker → OpenCode.
4. Port remaining vertical slices, including OAuth/browser protocols, operations, streaming routes, and Translator with a Rust textarea.
5. Perform one full cutover to Leptos, remove Astro/Node from the main build, and update build/release/docs.
6. Run closure verification, commit, push, deploy during a maintenance window, and verify health/core behavior.

## Current Checkpoint

- Closes: R1-R6.
- Smallest next action: none; closure check passed.
- Expected evidence: recorded below.
- Stop or replan if: not applicable; objective is complete.

## Current State

- Resolved: R1-R6: toolchain, Leptos dashboard/routes, shared model workflow, textarea Translator, Node-free main build, single-binary image, commit/push, and production deployment.
- Last relevant evidence: production Compose service is healthy on port 4623 and serves the Leptos shell plus hashed WASM; pre-deploy dashboard/backend/Docker/plugin checks are recorded in the prior checkpoint.
- Blocker: none.
- Next: none.

## Material Decisions

- 2026-09-16: Use Leptos CSR and Trunk; retain the existing Axum JSON API boundary.
- 2026-09-16: Raise the project Rust version from 1.85 to 1.98.
- 2026-09-16: Use one production cutover; no dual frontend or embedded Astro fallback.
- 2026-09-16: Replace Monaco with a Rust-rendered textarea preserving load/edit/save/translate.
- 2026-09-16: Node-free applies to runtime and the main build; the supported OpenCode JavaScript plugin remains.

## Checkpoint History

- 2026-09-16: Contract frozen from the user-approved audited plan; implementation not yet started.
- 2026-09-16: R1/R2 foundation checkpoint passed locally: Rust 1.98.1 pinned, backend check green, Leptos CSR release artifact built by Trunk.
- 2026-09-16: R2-R5 implementation checkpoint passed: Leptos routes and browser flows compile, shared model tests pass, Astro dashboard was removed, and the Node-free production image plus embedded/runtime smokes passed.
- 2026-09-16: R6 passed: pushed `0f4f6569`, backed up the production volume, retained rollback image `openproxy:rollback-pre-leptos`, recreated production Compose, and verified healthy Leptos static serving.

## Completion

- Resolved outcomes: R1-R6 verified.
- Commands and artifacts: Rust/dashboard checks and tests; Trunk release; OpenCode plugin tests; `docker compose build`; disposable image and core API smokes; commit `0f4f6569`; `docker compose up -d --no-build --force-recreate openproxy`; production `/health` and static/deep-link checks.
- Constraint and diff-scope check: one CSR dashboard and existing Axum API boundary; no schema migration, SSR, dual UI, committed generated dashboard output, local config, or secrets. OpenCode plugin retained.
- Final status: complete.
