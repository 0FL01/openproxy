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
  - Status: in_progress
  - Evidence: initial synthetic login/Endpoint parity scenarios are being prepared; none has passed or produced a reference yet.
- R3: Full Dioxus/Rust frontend preserves all current routes, layout, visual states, interactions, localization and API behavior.
  - Source: user request "фронтенд фулл на расте" and approved RECON plan.
  - Acceptance: all current routes and interactions match the reference, including authentication, OAuth callback, persisted model customizations and `/v1/models` discovery; browser application logic is Rust/WASM, with Dioxus-generated JS bridge and CSS allowed.
  - Primary evidence: R2 visual scenarios and existing focused frontend/API regression tests adapted to the new frontend.
  - Status: in_progress
  - Evidence: isolated Dioxus login slice builds, passes synthetic browser/auth tests and retains reference CSS/assets; it is not yet verified against Astro or a real isolated backend. All other routes remain pending.
- R4: Translator retains full actually available Monaco editor behavior and appearance without any third-party JavaScript editor.
  - Source: user selected "Полный паритет" after being informed that this excludes a third-party JS editor.
  - Acceptance: feature and visual parity against the configured existing Monaco across its available actions, JSON/plaintext and all seven Translator stages; no third-party JS editor ships.
  - Primary evidence: browser comparisons and action scenarios against the reference editor, including keyboard, selection, IME, history and accessibility.
  - Status: in_progress
  - Evidence: isolated `web/dist` Translator with local Monaco 0.55.1, synthetic APIs and sandboxed Chromium 153 measured 129 supported actions in a settled JSON model and 126 in plaintext; registration alone is not proof of behavior. Editing, undo/redo, find/replace and formatting were exercised. Monaco format used four spaces, while the page's Format button used two. Google Fonts were blocked, so no visual parity was established. No equivalent Rust editor was found; a basic syntax-highlighted textarea is not sufficient.
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
  - Status: in_progress
  - Evidence: goal commit `269d55fa` and login slice `27c38be6` were pushed to `origin/feat/dioxus-dashboard`; production was not deployed or modified.

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

- Closes: R1 and the first R2/R3 login-parity slice.
- Smallest next action: make sandboxed Chromium start in the isolated container, verify MCP screenshot, then record and compare the login reference and Dioxus slice.
- Expected evidence: browser/MCP screenshot on isolated data and reproducible visual/behavioral comparison on the same pinned browser.
- Stop or replan if: browser/reference constraints cannot be met without violating the frozen contract.

## Current State

- Resolved: none of the end-to-end outcomes yet.
- Last relevant evidence: Dioxus 0.7.10 release WASM build, three auth unit tests, WASM clippy/format and sandboxed synthetic login smoke passed; the latter reproduces and fixes empty-password submission parity. An Astro build in `/tmp/opencode` reproduced the existing login/Endpoint HTML, key bundles and CSS byte-for-byte. Baseline `cargo fmt`, all-target/all-feature clippy, eight provider-model unit tests, seven isolated custom-model API tests, an isolated auth API test and 25 OpenCode plugin tests passed. The isolated browser image and app start and OpenCode discovers MCP, but Chrome's sandboxed namespace creation failed with `Operation not permitted`; no MCP screenshot or visual comparison has passed yet.
- Blocker: browser namespace policy; user has authorized changing test-container permissions, and a narrowly scoped seccomp adjustment is being tried.
- Next: finish the browser smoke and visual comparison, then migrate the remaining sections and Translator editor.

## Material Decisions

- 2026-10-26: User selected full Monaco parity without a third-party JS editor. Dioxus-generated browser bridge is an unavoidable allowed runtime component.
- 2026-10-26: Use a feature branch for work and pushes; production deployment is expressly excluded.
- 2026-10-26: User authorized any necessary **test-container** permissions and requested no further questions. Prefer a browser-only minimal seccomp adjustment and keep Chromium sandbox, TLS verification, isolated network and production boundaries intact.
- 2026-10-26: An initial backend auth test inadvertently read the pre-existing host `~/.openproxy/api_key_secret`; the file timestamp was unchanged and its value was not output. It did not access the production container/volume. The test was repeated successfully with isolated `DATA_DIR`/`TMPDIR` and synthetic secrets; subsequent backend tests use the same isolation.

## Checkpoint History

- 2026-10-26: Frozen goal from user request and RECON; no implementation verified yet.
- 2026-10-26: R3 partial login slice built and synthetic behavior checked; R1 MCP catalog works but navigation is blocked by the container namespace policy. Visual parity remains unverified.
- 2026-10-26: R4 reference probe measured actual Monaco actions and representative behavior using synthetic data in a loopback-only network namespace; no editor implementation or parity is claimed.

## Completion

- Resolved outcomes: pending.
- Commands and artifacts: pending.
- Constraint and diff-scope check: pending.
- Final status: active.
