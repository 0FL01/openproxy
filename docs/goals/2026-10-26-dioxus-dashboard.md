# Goal: Pixel-perfect Rust dashboard

Status: active
Source: user request 2026-10-26 and approved RECON plan; user selected full Monaco parity without a third-party JavaScript editor; latest request: "итеративно реализовать и коммит пуш, в прод - не деплоить".
Last updated: 2026-10-27

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
  - Status: verified
  - Evidence: browser-only seccomp allows Chromium's namespace sandbox without `SYS_ADMIN`. The rebuilt project-local MCP uses an internal Docker network with isolated gateway; `python3 -B .ui-browser/smoke.py` authenticated to the synthetic app and captured a 1280×720 PNG, and the reconnected OpenCode MCP opened its login page. A CDP font test confirmed Latin and Cyrillic glyphs use pinned offline Inter subsets.
- R2: Preserve a reproducible visual and behavioral reference for every existing frontend section and significant state.
  - Source: "pixel perfect" and "сравнивать каждый пункт в разделе"; approved RECON plan.
  - Acceptance: synthetic fixtures, fixed browser/fonts/time/viewport/theme, reproducible self-comparison, and reference PNGs; candidate screenshots match at zero differing pixels apart from specifically documented unavoidable masks with independent content checks.
  - Primary evidence: repeatable browser parity scenarios for desktop and mobile, all sections and meaningful states.
  - Status: in_progress
  - Evidence: the harness pins Chrome and disables its nondeterministic partial rasterization. The Providers-list candidate matched 22 login, 38 Endpoint and 28 Providers-list synthetic reference screenshots at AE=0; the scrolling cases use overflowing fixtures. The newer Providers-new candidate matched all 30 new-page references and 12 representative prior shared-shell screenshots. The partial detail candidate matched all 32 Kilo Code references and 16 representative earlier shared-shell screenshots at AE=0 before subsequent OAuth-only cleanup changes. The callback candidate matched all ten standalone callback references at AE=0 after scoping CSS and correcting the fixture to stub the native Clipboard method; the two affected Astro references remained byte-identical. The compatible-node candidate matched all 16 synthetic reference screenshots at AE=0. The updated Codex candidate matched all eight auto-ping and four empty Bulk Add dialog references at AE=0. References and comparisons for other sections remain pending.
- R3: Full Dioxus/Rust frontend preserves all current routes, layout, visual states, interactions, localization and API behavior.
  - Source: user request "фронтенд фулл на расте" and approved RECON plan.
  - Acceptance: all current routes and interactions match the reference, including authentication, OAuth callback, persisted model customizations and `/v1/models` discovery; browser application logic is Rust/WASM, with Dioxus-generated JS bridge and CSS allowed.
  - Primary evidence: R2 visual scenarios and existing focused frontend/API regression tests adapted to the new frontend.
  - Status: in_progress
  - Evidence: the Dioxus login/shell/Endpoint slice matches all 60 R2 screenshots at AE=0 and passed an isolated real-backend session test including HttpOnly, Secure and SameSite=Lax cookie attributes, synthetic key creation and persistence after reload. The Providers list matches its 28 reference screenshots; synthetic behavior checks passed after correcting deep-link retention on session-check failure, supported large priorities, JavaScript search trimming, failed-load ordering and image color metadata in the documented build path. A real isolated backend test created and disabled a synthetic provider connection and verified persistence after reload; external validation was mocked and no provider was contacted. The Providers-new candidate matches 30 references; synthetic navigation/save tests passed, and its direct creation persisted after reload against the isolated backend. The partial detail candidate includes connection and model controls; an isolated real-backend test passed UI custom-model add, disable, reload and restore, including exclusion and reinclusion in `/v1/models`. Reproduced OAuth cancellation, unbounded wait and HTTP 504 fence regressions passed focused synthetic checks after fixes. The callback candidate passed synthetic same-origin delivery, nonce/state/origin rejection and no-opener popup/cleanup checks. Cline's live preservation of the nonce-bearing redirect URI is unverified, and cross-origin callbacks may need manual completion. Backend proxy stops remain provider-wide, so cross-tab and delayed in-flight cleanup cannot be guaranteed by this client alone. Compatible-node edit, import and delete passed synthetic browser checks; a real isolated backend verified UUID-node edit across reload and deletion without deleting its connection. The authenticated model-list route now resolves UUID-node type and URL, with local mock tests for node fallback, connection override and legacy IDs. Codex and GLM auto-ping passed focused synthetic behavior checks; an inactive synthetic GLM connection also persisted its auto-ping setting after reload against an isolated backend, without provider traffic. The Codex signup link, advertised context and Bulk Add passed synthetic browser behavior checks; an isolated real backend persisted the Codex context change after page reload. These checks do not prove live upstream import or persistence across a backend restart. Provider-specific detail controls and other routes remain incomplete. The 34 reference locale catalogs are bundled; Russian dashboard and Chinese login behavior were exercised synthetically.
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
  - Evidence: goal commit `269d55fa`, login slice `27c38be6`, checkpoint `da01a7b0`, isolated-browser fixture `2fb691fe`, visual references `e2514d20`, overflow-scenario correction `d8e98d88`, shell/Endpoint slice `c36f3e6f`, Providers-list references `7f34767e`, Providers-new references `eaf309c6`, Providers-list slice `b4bc66b8`, Providers-new slice `9ef2e9ac`, detail references `5050018a`, partial detail slice `07880ba0`, callback references `f0e6d88f`, callback relay `71e99ce2`, compatible-node references `14411591`, compatible-node slice `110b51b5`, auto-ping references `8328ff7c`, auto-ping controls `c1a60195` and Codex bulk-dialog references `e709d3cc` were pushed to `origin/feat/dioxus-dashboard`; production was not deployed or modified.

### Constraints and Non-goals

- Never deploy, stop, rebuild or otherwise change the production Compose service, image tag, configuration or `openproxy-prod-data`; never use production secrets/data in tests.
- Preserve `contracts/lean-proxy.md`, v1 API/auth/OAuth contracts, provider model user data, accessibility and concurrency; do not reintroduce retired features.
- Do not commit `opencode.json(c)`, `.env` files, live credentials, private test data or generated local artifacts. Synthetic reference fixtures are allowed.
- Do not disable browser TLS verification or sandbox as an automatic workaround; do not expose CDP or grant broad host privileges.
- No new backend service or architecture solely for the frontend. Generated Dioxus browser JavaScript is allowed; third-party JavaScript editors are not.

## Change Envelope

- Targets: `.ui-browser/`, local ignored `opencode.jsonc`, synthetic browser references/tests, new `frontend/` Dioxus crate, existing dashboard packaging/hosting and focused tests/docs when their contract requires changes.
- Direct consumers: `src/server/dashboard/mod.rs`, `build.rs`, `web/dist`, Dockerfile, CI/release workflows, dashboard API/auth and `plugins/openproxy-models.js`.
- R3 blocker: the new compatible-node import calls the existing model-list API, whose dispatch recognizes only prefixed provider IDs even though node creation produces UUIDs; its fetcher also expects a connection-level URL while the UI stores it on the node. Verify this path with a local mock, then, if reproduced, allow the existing fetch owner and its focused test to resolve the persisted node type and URL as the chat executor does. Do not change the API, stored data or URL/security policy.
- Keep old Astro frontend and production builds intact during incremental work; cut over only once parity is established. Test data and browser profiles are disposable.
- Work on branch `feat/dioxus-dashboard`; push verified commits to `origin` without touching production.

## Current Checkpoint

- Closes: the next R2/R3 route slice.
- Smallest next action: complete Quota Tracker's provider-specific quota formatting using existing APIs.
- Expected evidence: focused synthetic UI behavior and a comparison with the reference.
- Stop or replan if: the route cannot be preserved within the frozen frontend and API boundaries.

## Current State

- Resolved: R1; fixture commit `2fb691fe` is pushed.
- Last relevant evidence: the Providers-list candidate matched all 88 login, Endpoint and Providers-list screenshots at AE=0; the newer candidate matched all 30 new-provider screenshots and 12 representative shared-shell screenshots. The partial detail candidate matched 32 Kilo Code and 16 representative earlier screenshots at AE=0 and passed the isolated custom-model persistence/discovery test; subsequent OAuth-only changes passed focused synthetic cancellation and HTTP-failure checks. The callback candidate matched all ten callback and 16 representative prior screenshots at AE=0. The latest compatible-node candidate matched 16 screenshots at AE=0 and passed isolated real-backend edit/reload/delete with connection retention. The UUID and legacy model-list tests passed with a local mock and isolated database. The Codex candidate matched eight auto-ping and four Bulk Add dialog screenshots at AE=0; its context change persisted after reload against an isolated backend, and the focused synthetic Codex controls smoke passed.
- Context controls: the focused synthetic browser check passed for OpenCode Go, GLM and Claude, including settings preservation, reload and catalog refresh; OpenCode Zen remains hidden as in the no-auth reference. A separate isolated backend check verified Codex and GLM settings across page reload. No new screenshot comparison or backend process-restart durability check was run.
- OpenRouter suggestions: the Astro reference self-repeated and Dioxus matched four synthetic desktop/mobile light/dark screenshots at AE=0 without masks. Both passed catalog/alias filtering, model-add request and page-reload checks on synthetic data; live OpenRouter and backend-restart persistence were not tested.
- Skills: the Dioxus page and clipboard feedback matched eight self-repeating Astro desktop/mobile light/dark screenshots at AE=0 without masks; the synthetic browser verified the copied URL, links and feedback reset.
- Console Log: eight self-repeating Astro desktop/mobile light/dark screenshots matched at AE=0 without masks. The synthetic browser exercised native EventSource init/line, the 200-line bound and autoscroll, then verified a mocked DELETE followed by a synthetic clear event. No real logs were read.
- xAI: twelve force-refresh and twenty diagnostic desktop/mobile light/dark screenshots matched self-repeating Astro references at AE=0 without masks. Focused synthetic checks covered canonical connection selection, concurrent catalog reloads, saved-model preservation and failure/session handling. Live xAI was not contacted.
- Profile: 36 self-repeating Astro desktop/mobile light/dark card, modal and full-page screenshots matched at AE=0 without masks. Synthetic checks exercised settings and account actions. A fresh isolated backend verified database export/import rejects an incorrect reauthentication password and accepts the synthetic correct password; no real data was used.
- DB Backups: 16 self-repeating Astro desktop/mobile light/dark screenshots matched at AE=0 without masks. Synthetic checks exercised snapshot, restore, delete, prune, export and multipart import. A fresh isolated backend verified snapshot creation and wrong/correct password reauthentication for export and import; no real data was used.
- Application Logs: 16 self-repeating Astro desktop/mobile light/dark screenshots for populated and empty logs, request details and expanded stream trace matched at AE=0 without masks. The synthetic TPS browser regression passed filtering, pagination, exclusions, persistence, visibility, polling, failure and recovery checks; no real logs or providers were accessed.
- Quota Tracker (partial): eight self-repeating Astro desktop/mobile light/dark screenshots for empty and one synthetic GitHub account matched at AE=0 without masks. Four more synthetic GLM auto-ping screenshots matched at AE=0; browser checks covered both GLM and Codex eligibility, settings preservation, reload and failure rollback. Twelve synthetic Codex reset-credit card, confirmation and expiry screenshots matched at AE=0; browser checks covered lookup/consume failures, cancellation, pending controls and confirmed reset. Four reset-time screenshots also matched at AE=0, with Today/Tomorrow/dated tooltips and recurring/expiry labels checked using a fixed clock. Provider-specific formatting remains incomplete; no live provider was contacted.
- Proxy Pools: sixteen self-repeating Astro desktop/mobile light/dark screenshots for empty, populated, selected and add-dialog states matched at AE=0 without masks. Synthetic browser checks covered CRUD, import, bulk and health actions, mocked relay deployment and session recheck; no real proxy or external relay was contacted.
- Blocker: none external. Dioxus re-encodes PNG color metadata; `frontend/scripts/build.sh` preserves original provider icons for the served output. The skeleton and theme cannot appear before WASM startup as Astro's synchronous HTML/script did.
- Next: complete Quota Tracker, migrate the other missing routes and remaining provider-specific behavior, and implement the Translator editor. Cline's live query-preservation behavior remains unverified.

## Material Decisions

- 2026-10-26: User selected full Monaco parity without a third-party JS editor. Dioxus-generated browser bridge is an unavoidable allowed runtime component.
- 2026-10-26: Use a feature branch for work and pushes; production deployment is expressly excluded.
- 2026-10-26: User authorized any necessary **test-container** permissions and requested no further questions. Prefer a browser-only minimal seccomp adjustment and keep Chromium sandbox, TLS verification, isolated network and production boundaries intact.
- 2026-10-26: An initial backend auth test inadvertently read the pre-existing host `~/.openproxy/api_key_secret`; the file timestamp was unchanged and its value was not output. It did not access the production container/volume. The test was repeated successfully with isolated `DATA_DIR`/`TMPDIR` and synthetic secrets; subsequent backend tests use the same isolation.

## Checkpoint History

- 2026-10-26: Frozen goal from user request and RECON; no implementation verified yet.
- 2026-10-26: R3 partial login slice built and synthetic behavior checked; R1 MCP catalog works but navigation is blocked by the container namespace policy. Visual parity remains unverified.
- 2026-10-26: R4 reference probe measured actual Monaco actions and representative behavior using synthetic data in a loopback-only network namespace; no editor implementation or parity is claimed.
- 2026-10-26: R1 MCP screenshot succeeded with the sandboxed fixture. R2/R3: 22 login screenshots matched and an isolated real-backend login passed; the browser fixture has since changed and needs revalidation. Endpoint reference self-comparison exposed a 21-pixel nondeterminism, without a mask or parity claim.
- 2026-10-26: R1 fixture verified and pushed in `2fb691fe`. Chrome's partial rasterization was isolated as the likely cause of reference-edge instability; the comparison harness pins the corrected browser flags and rejects incompatible reference metadata. Endpoint differences remain under investigation.
- 2026-10-26: R2/R3 login and Endpoint slices matched all 60 synthetic references at AE=0 after restoring the reference skeleton. Real isolated login/key persistence and frontend browser checks passed; the slice was pushed in `c36f3e6f`. Providers is the next checkpoint.
- 2026-10-26: R2/R3 Providers-list candidate matched 28 synthetic reference screenshots at AE=0 and retained all 60 earlier matches. Synthetic provider creation and disabling persisted against the isolated backend with mocked upstream validation; the slice was pushed in `b4bc66b8`. Thirty new-provider reference screenshots self-repeated at AE=0; their Dioxus implementation is pending.
- 2026-10-26: R2/R3 Providers-new candidate matched 30 new-page and 12 representative shared-shell screenshots at AE=0. Independent synthetic behavior review and the real isolated-backend save/persistence test passed; the slice was pushed in `9ef2e9ac`. Provider detail and model customization remain the next slice.
- 2026-10-26: R3 partial detail candidate passed a real isolated-backend custom-model add, Disable All, page reload, `/v1/models` exclusion and UI restore/discovery regression. The synthetic backend had stdout/stderr redirected and Docker logging disabled; the disposable project was removed. Detail regressions and visual parity remain unresolved.
- 2026-10-26: R2 Kilo Code detail reference recorded 32 synthetic screenshots for connections and model customization; two independent recordings self-repeated at AE=0 and were pushed in `5050018a`. The partial Dioxus detail candidate timed out before capture on its missing connection-selection control; its real-backend test is in progress.
- 2026-10-27: R3 partial detail fixed the four reproduced behavioral regressions and added connection selection. Its real isolated-backend model-persistence/discovery test passed after allowing the new exact proxy-pool read; four ready-state screenshots still differ, with missing reference controls and header. The slice remains uncommitted and incomplete.
- 2026-10-27: R2/R3 updated partial detail matched all 32 Kilo Code and 16 representative earlier synthetic screenshots at AE=0; the isolated custom-model add, disable, reload, restore and `/v1/models` discovery regression passed after connection and OAuth fixes. Independent OAuth verification remains pending.
- 2026-10-27: Independent synthetic OAuth verification passed the earlier five fixes and the active-pool case, but reproduced delayed listener cleanup after final cancellation and an unbounded stop wait. Both remain within the current partial detail checkpoint.
- 2026-10-27: Focused OAuth smoke passed after fixing final-cancellation cleanup, bounding the stop wait and fencing non-success HTTP stops. The provider-wide backend listener cannot provide cross-tab or delayed-start guarantees; this is documented without changing its API.
- 2026-10-27: The partial provider-detail slice was pushed in `07880ba0`. The next checkpoint is the standalone OAuth callback and its popup delivery channels.
- 2026-10-27: R2 callback references cover ten synthetic desktop/mobile states; two independent recordings self-repeated at AE=0 with both OS color schemes. Dioxus callback and delivery behavior remain to be verified.
- 2026-10-27: R2/R3 callback candidate matched ten screenshots and 16 representative prior screenshots at AE=0. Synthetic browser checks verified same-origin callback transports, nonce/state/origin rejection and no-opener popup delivery, plus late exchange and popup-blocker behavior. Cline query preservation has not been tested against the live provider.
- 2026-10-27: R2/R3 compatible-node candidate matched 16 screenshots at AE=0; synthetic import and edit regressions passed. A real isolated backend verified UUID-node edit, page-reload persistence and deletion without cascading to its connection. The model-list backend fix passed UUID fallback/override and legacy-prefix tests against a local mock. Live upstream import and backend-restart durability were not tested.
- 2026-10-27: Codex/GLM auto-ping passed synthetic behavior checks and persisted for an inactive GLM connection across a page reload on an isolated backend. Eight Codex references self-repeated at AE=0; all eight candidate full-page comparisons failed, with previously unmigrated Codex controls visible in the reference. Visual parity remains pending.
- 2026-10-27: R2/R3 Codex signup, advertised context and Bulk Add were implemented. The updated candidate matched eight auto-ping and four empty Bulk Add dialog screenshots at AE=0; the synthetic controls smoke and isolated real-backend context save/reload passed. Live upstream import and backend-restart durability were not tested.
- 2026-10-27: R3 advertised-context controls were extended to the other applicable providers. The focused synthetic browser check and an isolated backend settings save/reload check passed; screenshots and backend-restart durability were not checked in this slice.
- 2026-10-27: R2/R3 OpenRouter suggested-model chips and their add behavior matched four self-repeating synthetic Astro references at AE=0; no live provider was contacted.
- 2026-10-27: R2/R3 Skills page matched eight self-repeating synthetic Astro references at AE=0; clipboard and link behavior were checked in the isolated browser.
- 2026-10-27: R2/R3 Console Log matched eight self-repeating synthetic Astro references at AE=0; stream and clear-event behavior were checked without reading real logs.
- 2026-10-27: R2/R3 xAI force-refresh ready, pending and failure states matched twelve self-repeating synthetic Astro references at AE=0. Focused synthetic behavior checks passed; no live provider was contacted.
- 2026-10-27: R2/R3 xAI stale, unavailable, empty, no-connection and connection-error diagnostics matched twenty self-repeating synthetic Astro references at AE=0. Shared connection and force-refresh browser smokes passed.
- 2026-10-27: R2/R3 Profile settings matched 36 self-repeating synthetic Astro references at AE=0; its database backup reauthentication passed against a fresh isolated backend.
- 2026-10-27: R2/R3 DB Backups matched 16 self-repeating synthetic Astro references at AE=0; snapshot creation and export/import reauthentication passed against a fresh isolated backend.
- 2026-10-27: R2/R3 Application Logs matched 16 self-repeating synthetic Astro references at AE=0; the focused synthetic TPS browser regression passed without reading real logs.
- 2026-10-27: R2/R3 partial Quota Tracker matched eight self-repeating synthetic Astro screenshots at AE=0; connected-account refresh/filter/toggle behavior passed. Provider-specific controls and formatting remain to migrate.
- 2026-10-27: R2/R3 Proxy Pools matched sixteen self-repeating synthetic Astro references at AE=0; its synthetic CRUD, batch, health and mocked relay browser smoke passed.
- 2026-10-27: R2/R3 Quota Tracker auto-ping was added for eligible Codex and GLM connections. Four synthetic GLM screenshots matched self-repeating Astro at AE=0; Codex eligibility and settings persistence were checked separately.
- 2026-10-27: R2/R3 Quota Tracker reset-time labels and tooltips matched four self-repeating synthetic Astro screenshots at AE=0; the clock was fixed and no live provider was contacted.
- 2026-10-27: R2/R3 Codex reset-credit controls matched twelve self-repeating synthetic Astro screenshots at AE=0; lookup, confirmation, failure and reset behavior were exercised without contacting Codex.

## Completion

- Resolved outcomes: pending.
- Commands and artifacts: pending.
- Constraint and diff-scope check: pending.
- Final status: active.
