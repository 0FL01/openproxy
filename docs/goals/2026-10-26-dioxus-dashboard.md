# Goal: Pixel-perfect Rust dashboard

Status: active
Source: user request 2026-10-26 and approved RECON plan; user selected full Monaco parity without a third-party JavaScript editor; latest request: "итеративно реализовать и коммит пуш, в прод - не деплоить".
Last updated: 2026-10-26

## Objective

Replace the existing OpenProxy frontend with a Dioxus/Rust web frontend that reproduces its appearance and supported behavior, verified against the current frontend in an isolated browser, then commit and push the verified work without deploying production.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Stop substantive work at a proven external blocker, an approved budget boundary, or when no remaining in-scope action has a falsifiable expected result; record the exact evidence and smallest unlock.

## Frozen Contract

### Required Outcomes

- R1: Local containerized Chrome DevTools MCP and isolated test OpenProxy can be used through project-local OpenCode configuration.
  - Source: user request and approved browser plan.
  - Acceptance: non-root sandboxed browser, stdio MCP, no externally published CDP; test app and synthetic data isolated from production; MCP can load and capture the test UI.
  - Primary evidence: MCP discovery and screenshot smoke against the isolated app.
  - Status: in_progress
  - Evidence: pending.
- R2: Preserve a reproducible visual and behavioral reference for every existing frontend section and significant state.
  - Source: "pixel perfect" and "сравнивать каждый пункт в разделе"; approved RECON plan.
  - Acceptance: synthetic fixtures, fixed browser/fonts/time/viewport/theme, reproducible self-comparison, and reference PNGs; candidate screenshots match at zero differing pixels apart from specifically documented unavoidable masks with independent content checks.
  - Primary evidence: repeatable browser parity scenarios for desktop and mobile, all sections and meaningful states.
  - Status: pending
  - Evidence: pending.
- R3: Full Dioxus/Rust frontend preserves all current routes, layout, visual states, interactions, localization and API behavior.
  - Source: user request "фронтенд фулл на расте" and approved RECON plan.
  - Acceptance: all current routes and interactions match the reference, including authentication, OAuth callback, persisted model customizations and `/v1/models` discovery; browser application logic is Rust/WASM, with Dioxus-generated JS bridge and CSS allowed.
  - Primary evidence: R2 visual scenarios and existing focused frontend/API regression tests adapted to the new frontend.
  - Status: pending
  - Evidence: pending.
- R4: Translator retains full actually available Monaco editor behavior and appearance without any third-party JavaScript editor.
  - Source: user selected "Полный паритет" after being informed that this excludes a third-party JS editor.
  - Acceptance: feature and visual parity against the configured existing Monaco across its available actions, JSON/plaintext and all seven Translator stages; no third-party JS editor ships.
  - Primary evidence: browser comparisons and action scenarios against the reference editor, including keyboard, selection, IME, history and accessibility.
  - Status: pending
  - Evidence: pending. No equivalent Rust editor was established during RECON; a basic syntax-highlighted textarea is not sufficient.
- R5: Standard builds package the Dioxus frontend into the existing OpenProxy binary without stale assets or breaking dashboard/API serving contracts.
  - Source: full frontend replacement and approved RECON plan.
  - Acceptance: `web/dist` contains only the new build, embedded/disk dashboard modes, deep links and WASM MIME/cache behavior work; Docker, CI and release builds use the new frontend; existing frozen backend/API and plugin contracts remain intact.
  - Primary evidence: frontend release build, focused dashboard/server/API tests, existing plugin tests and affected CI build checks.
  - Status: pending
  - Evidence: pending.
- R6: Commit and push the verified iterative work without deploying to production.
  - Source: user request 2026-10-26.
  - Acceptance: intended commits are present on the remote working branch; no production service, data, configuration or deployment is changed.
  - Primary evidence: git commit and remote branch SHA; isolated test deployment only.
  - Status: pending
  - Evidence: pending.

### Constraints and Non-goals

- Never deploy, stop, rebuild or otherwise change the production Compose service, image tag, configuration or `openproxy-prod-data`; never use production secrets/data in tests.
- Preserve `contracts/lean-proxy.md`, v1 API/auth/OAuth contracts, provider model user data, accessibility and concurrency; do not reintroduce retired features.
- Do not commit `opencode.json(c)`, `.env` files, live credentials, private test data or generated local artifacts. Synthetic reference fixtures are allowed.
- Do not disable browser TLS verification or sandbox as an automatic workaround; do not expose CDP or grant broad host privileges.
- No new backend service or architecture solely for the frontend. Generated Dioxus browser JavaScript is allowed; third-party JavaScript editors are not.

## Change Envelope

- Targets: `.ui-browser/`, local ignored `opencode.jsonc`, synthetic browser references/tests, new `frontend/` Dioxus crate, existing dashboard packaging/hosting and focused tests/docs when their contract requires changes.
- Direct consumers: `src/server/dashboard/mod.rs`, `build.rs`, `web/dist`, Dockerfile, CI/release workflows, dashboard API/auth and `plugins/openproxy-models.js`.
- Keep old Astro frontend and production builds intact during incremental work; cut over only once parity is established. Test data and browser profiles are disposable.
- Work on branch `feat/dioxus-dashboard`; push verified commits to `origin` without touching production.

## Current Checkpoint

- Closes: R1 and the smallest initial R3 frontend build slice, independently.
- Smallest next action: prepare the isolated MCP/browser test environment and a buildable Dioxus web application; record exact smoke evidence before extending either slice.
- Expected evidence: browser/MCP screenshot on isolated data and a successful Dioxus WASM build.
- Stop or replan if: sandbox, build or browser/reference constraints cannot be met without violating the frozen contract.

## Current State

- Resolved: none.
- Last relevant evidence: read-only RECON; working tree was clean on `main` at `82c48877`; `origin` is the OpenProxy fork and work is on `feat/dioxus-dashboard`. Rust 1.98.1, wasm target, Node 22 and Docker are installed; `dx` is not installed.
- Blocker: none established.
- Next: implement and verify the current checkpoint; then capture synthetic baseline and proceed section by section.

## Material Decisions

- 2026-10-26: User selected full Monaco parity without a third-party JS editor. Dioxus-generated browser bridge is an unavoidable allowed runtime component.
- 2026-10-26: Use a feature branch for work and pushes; production deployment is expressly excluded.

## Checkpoint History

- 2026-10-26: Frozen goal from user request and RECON; no implementation verified yet.

## Completion

- Resolved outcomes: pending.
- Commands and artifacts: pending.
- Constraint and diff-scope check: pending.
- Final status: active.
