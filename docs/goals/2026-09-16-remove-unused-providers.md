# Goal: Remove unused providers (full cleanup with roots)

Status: complete
Source: User-approved audited plan on 2026-09-16 (5 parallel @general reviews, all approve-with-corrections, synthesized); implement iteratively, commit, push, and deploy.
Last updated: 2026-09-16

## Objective

Shrink OpenProxy to the actually used provider stack — `codex`, `opencode-go`/`opencode-zen`, and the generic `DefaultExecutor`/`ApiKeyExecutor` path (glm/z.ai, kimi, and any OpenAI-compatible key) — plus kept frontier/local transports (`antigravity`, `grok-web`, plain `gemini`/`perplexity` API). Delete 4 specialized executors (Perplexity Web, Qoder, IFlow, Gemini CLI) and ~25 catalog-only providers end to end: executors, OAuth, translator variants, dispatch, dashboard, CLI, seed data (`provider_catalog.json` + `sources/*.json` + vendored `web/open-sse` mirror), tests, icons. ~9.5–10k lines. No "provider is removed" tests — just delete.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Commit each independently buildable removal before starting the next. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes

- R1: Remove Perplexity Web (Pro/Max), keep GrokWeb intact.
  - Source: Approved plan Part 1 + review corrections (R1-item: shared code inside deletion range).
  - Acceptance: `PerplexityWeb*` gone; `grok_web.rs` keeps lines 1381–1644 (`sse_chunk`, `GrokEvent`/`parse_grok_line`, `convert_grok_response`, `json_error` shared with GrokWeb); deleted `575–1380` + `1645–2008`, doc `:17–18`, import cleanup (`HashMap`/`Mutex`/`SystemTime`/`AUTHORIZATION`); `mod.rs:79–80`, `chat.rs` import + dispatch `:1404–1426` + exempt list `:2041`, `default.rs:279`, conn-test `:731–733` + `:982–1027`, `api/mod.rs:2170`, `model/mod.rs:64–65`, `providers.ts:195`, catalog `:57,:2683–2719,:6331–6339`, sources `9router :58,:4527–4570` + `omniroute :132,:6955–7002` (else `openproxy sync` re-seeds), open-sse entries, pplx test fns deleted (grok tests kept), baseline `:1265` hygiene. Plain `perplexity`/`perplexity-agent` API path and `capabilities.rs:620` `*pplx*` glob stay.
  - Primary evidence: `cargo check/clippy`, grok-web tests, `rg -i perplexity-web|pplx` shows only plain-API hits, dashboard build.
  - Status: verified
  - Evidence: PerplexityWeb section cut (575–1380, 1645–2008) with GrokWeb keep-range 1381–1644 intact; dispatch/conn-test/cookie entries/catalog+sources/dashboard cleaned. Gates: fmt, clippy all-targets, grok_web unit tests (13), opencode model suites, pnpm build (123 pages).

- R2: Remove Qoder end to end.
  - Source: Approved plan Part 2 + review corrections (ranges, orphan module, data objects).
  - Acceptance: Files `executor/qoder.rs` (3580) + `oauth/qoder.rs` (342, orphan — no `mod` decl) deleted; `mod.rs:24,:100`; `chat.rs` import + dispatch `:1290–1319` + SSE helpers `:3129–3182` + `:3184–3196` + state refs; `providers.rs:300–306,:553`; `token_refresh.rs:915–917`; `secret.rs:31–33`; `oauth.rs:700` entry; `provider_models.rs:320,:478–482`; `usage.rs` import/arms; `quota_fetcher.rs:1379–1465`; dashboard `providers.ts:17,:606`, `OAuthModal` + comment `:194`, detail-page Qoder blocks; catalog `:9`; whole provider objects in `sources/*.json` (not just headers); CHANGELOG history untouched. `rg -i qoder` empty (excluding history).
  - Primary evidence: `cargo check/clippy`, full test suite (no qoder integration tests exist), repo search, dashboard build.
  - Status: verified
  - Evidence: qoder.rs (3580) + oauth/qoder.rs (342) deleted; SSE helpers, quota fn, discovery/usage arms, dashboard import button removed; catalog+sources stripped. An over-deletion of ProviderDetailPageClient (~530 lines) was caught by diff review, reverted, and redone as 3 precise deletions (−76). Gates: fmt, clippy all-targets, lib 1258 passed, pnpm build (122 pages).

- R3: Remove IFlow end to end.
  - Source: Approved plan Part 3 + review corrections (missed roots, schema note).
  - Acceptance: Files `executor/iflow.rs` (200) + `IFlowCookieModal.tsx` (131) + `tests/oauth_iflow_cookie_api.rs` (241) deleted; `mod.rs:16,:82`; `chat.rs` import + dispatch `1192–~1216`; `providers.rs` extra-params/fn/dispatch; `token_refresh.rs` consts + `refresh_iflow_token` + dispatch; `secret.rs` fn + tests; `background_refresh.rs:32`; `app_constants.rs:167,:207–209`; CLI `IflowCookie` variant + runner + `openproxy.v1.oauth.iflow_cookie` (record whole-resource removal in CHANGELOG — frozen v1 forbids silent removals); `oauth.rs` cookie fn `1775–~2004` + route `:6670` + compat exchange `:4221–4310` + helpers/dispatch; conn-test probe; `usage.rs:82`; `model/mod.rs:17`; dashboard entry + modal wiring + `index.ts:28`; `.env.example:83–86`; open-sse `providers.js:84–91` (incl. hardcoded clientSecret) + `providerModels.js:76,91,650`; catalog `:8,:368–369,:5306`; baseline `:52–63` hygiene; sources stripped; compat tests `173–192` + `456–532` removed; shared fns (`build_auth_compat_response`, `resolve()`, etc.) kept; generated i18n literals untouched.
  - Primary evidence: `cargo check/clippy`, oauth compat suites, repo search, dashboard build.
  - Status: verified
  - Evidence: iflow executor/modal/cookie-test deleted; cookie route + compat exchange + CLI command (openproxy.v1.oauth.iflow_cookie removal recorded in commit message) + refresh + dashboard wiring removed; qoder open-sse leftover folded in. Gates: fmt, clippy all-targets, oauth lib (120) + compat (6), pnpm build (121 pages).

- R4: Remove Gemini CLI last, keep Antigravity + plain Gemini intact.
  - Source: Approved plan Part 4 + review corrections (HIGH-risk translator boundary, missed roots).
  - Acceptance: Files `executor/gemini_cli.rs` (602) + `oauth/gemini_cli.rs` (480) deleted; `mod.rs`; `chat.rs` dispatch; providers/secret consts; token_refresh dispatch arm only (`refresh_google_token`, `GOOGLE_TOKEN_URL`, `build_google_auth_url`, `gemini_token_url`, `google_oauth_client_metadata`, `extract_google_project_id` stay — Antigravity uses them); `background_refresh` variant only; translator: delete ONLY `openai_to_gemini.rs:962–1015` (antigravity fn `:1017–1091` + tests stay — violating this kills Antigravity), registry arms incl. `:1009–1013`, `Format::GeminiCli` variant + `get_target_format_for_provider:367` + `needs_image_prefetch:97`, merge `thinking_suffix:152` / `stream_flags:57` arms, fix stale `antigravity.rs:693` comment; `app_constants:20–45`; provider_models fns + tests; conn-test arms (shared probe: variant only); `usage.rs`; `quota_fetcher:1212–1379`; `oauth.rs` compat arm + exchange fn (shared fns stay); `api/mod.rs:645`; `model/mod.rs:15` (no silent `gc` repoint); `client_detector.rs` variant + arms + tests; `default_thinking_signature.rs:19`; `token_refresh.rs:182` + doc `:355`; dashboard `providers.ts:14,:559,:585`; catalog `:5,:5284`; `9router.json:29`; gemini-compat test fns deleted (antigravity/iflow/cline kept); baseline `:39` hygiene; open-sse explicitly left + noted (reference-only mirror). Existing SQLite rows with `provider='gemini-cli'` degrade to `_ => OpenAi` + DefaultExecutor — noted in commit.
  - Primary evidence: `cargo check/clippy`, antigravity tests, translator registry tests, repo search, dashboard build.
  - Status: verified
  - Evidence: gemini-cli executor/oauth deleted with Format::GeminiCli arms; translator cut bounded to openai_to_gemini.rs:962–1015 (antigravity fn kept); shared Google helpers retained; a wrongly deleted shared load_code_assist helper (used by antigravity quota) was restored. One flaky lib failure matched the known intermittent SQLite encryption flake; two repeat runs green (1243). Gates: fmt, clippy all-targets, lib 1243 x2, translator (328) + antigravity suites, pnpm build (120 pages).

- R5: Remove catalog-only tail (25 aliases).
  - Source: Approved plan Part 5 + review corrections (missed block/token/PNGs, thinkingLevels ban).
  - Acceptance: `blackbox, byteplus, agentrouter, aimlapi, api-airforce, baidu, baseten, bazaarlink, chutes, bytez, bluesminds, completions, freetheai, nlpcloud, morph, poolside, predibase, publicai, puter, reka, uncloseai, tencent, siliconflow, kluster, groq` removed from `provider_catalog.json` (incl. missed `ar :204–263`), `default.rs PROVIDER_CONFIGS`, `provider_validate.rs` (incl. missed byteplus `:348` token; agentrouter: 6 shared-arm tokens only, arms stay + validate block `:118–129`), `provider_models.rs` discovery/fetch, conn-test arms, `model/mod.rs` 7 alias lines, `capabilities.rs:194–197` (poolside only), `translator.rs:256`, `providers.ts` entries + groq free-tier + discovery tokens + byteplus `config.ts:77`, 23 orphan PNGs in `web/public/providers/`; tests updated in same commit: `catalog.rs` parity (~15 lines), `executor_pool_behavior.rs` (~90–110 lines, groq fixtures renamed to a kept provider); `nslale` confirmed nonexistent — skipped (`nscale` out of scope). `thinkingLevels.ts` + hunyuan `capabilities.rs` lines NOT touched (provider-agnostic format infra). `api_key.rs` dead map + `sources/*.json` resurrection vector: skipped for code, noted in commit message. `docs/` zero hits.
  - Primary evidence: `cargo test` (catalog parity, pool behavior, api_auth, cloud_credentials), repo search, dashboard build + ModelSelectModal consistency.
  - Status: verified
  - Evidence: 25 aliases removed from default.rs, validate, discovery/fetch, conn-test, alias map, dashboard (+groq free-tier, discovery tokens, byteplus config), 23 PNGs, catalog/sources JSON (set-difference verified: only targets removed); agentrouter shared arms kept; thinkingLevels/hunyuan infra untouched; nslale confirmed nonexistent. A pollinations alias line clobbered by an off-by-one was restored. Gates: fmt, clippy all-targets, lib 1243, executor_pool (37) + api_auth (19) + cloud_credentials (7) + oauth compat (4), opencode suites, pnpm build (95 pages).

- R6: Deliver iteratively and safely.
  - Source: "Формируй goal с копией плана, далее переноси текущие правки в main ветку и начинай итеративную реализацию цели и потом коммит пуш деплой".
  - Acceptance: R1–R5 land as independently buildable Conventional Commits on `main` in order perplexity → qoder → iflow → gemini-cli → tail; per-commit gates (`fmt --check`, `clippy --all-targets --all-features`, affected tests, dashboard build where touched); final full gate; `main` pushed to `origin`; production rebuilt via `docker compose up -d --build` reusing `openproxy-prod-data`; `/health` ok + smoke (models listing, direct glm chat).
  - Primary evidence: Git history/push output, gate outputs, `docker compose ps`, `curl -fsS http://127.0.0.1:4623/health`.
  - Status: pending

### Constraints

- C1: Keep codex, opencode-go/zen, Default/ApiKey generic path (glm/z.ai), antigravity, grok-web, plain gemini/perplexity API, `client_pool`, `strip_unsupported`, `project_id_cache`, shared OAuth helpers. Existing user connections/accounts for kept providers keep working.
- C2: Preserve frozen `openproxy.v1.*` additive-only contract — whole-resource removal (`oauth.iflow_cookie`) recorded in CHANGELOG, not silent.
- C3: Preserve production secrets, encryption key, persistent volume `openproxy-prod-data`; never stage `.env*`, runtime data, backups.
- C4: No new tests proving removed providers are gone; update affected tests in the same commit; never leave `cargo test` red.
- C5: `ModelSelectModal` must mirror provider Available Models after dashboard constant removals.

### Non-goals

- Removing/replacing kept providers (antigravity, grok-web/claude-cli surface, vertex/azure/ollama/xai executors — separate decision).
- `nscale` (out of scope, `nslale` does not exist).
- Redesigning OAuth compat-exchange, quota fetching, or dashboard provider pages beyond deleted blocks.
- Rewriting historical changelog/goal records; touching generated i18n literals.

## Change Envelope

- Target: `src/core/executor/{qoder,iflow,gemini_cli,grok_web}`, `src/oauth/{qoder,gemini_cli}.rs` + provider/refresh/secret/background entries, `src/core/translator` GeminiCli arms, `src/server/api/{chat,oauth,provider_models,provider_connection_test,usage,mod}.rs` arms, `src/core/{model,usage,config,utils}` entries, `src/cli/provider_oauth.rs` IFlow variant, `src/core/executor/default.rs` + `provider_validate.rs` generic lists, `provider_catalog.json` + `sources/*.json`, `web/open-sse/config/*.js` entries, `web/src` provider constants/modal wiring, orphan PNGs, `.env.example`, affected tests, this goal document.
- Expected paths/symbols/consumers: listed per outcome above; nearest tests are co-located unit tests + `tests/oauth_*`, `tests/executor_pool_behavior.rs`, catalog parity tests, `pnpm build`, OpenCode model-discovery suites.
- Allowed artifacts: Deletions, minimal import/arm cleanups for retained shared helpers, same-commit affected-test updates, this goal document, CHANGELOG whole-resource note.
- Forbidden artifacts: New dependencies, new abstractions, removal-assertion tests, unrelated refactors/formatting, secrets, local config, database files, backups.
- User or harness budget: 5 independently buildable commits in fixed order, then push + deploy.

## Current Checkpoint

- Closes: R1-R6
- Smallest next action: Push `main` and deploy production (R6 delivery).
- Expected evidence: Push output, `docker compose ps`, `/health`, model listing smoke.
- Stop or replan if: Production health or smoke fails — roll back to pre-goal image.

## Current State

- Resolved: R1-R5 verified; implementation complete on `main`.
- Last relevant evidence: R5 gates green; 5 removal commits ahead of origin.
- Blocker: None.
- Next: Push + deploy.

## Material Decisions

- 2026-09-16: `sources/*.json` entries are stripped too — `openproxy sync` would otherwise re-seed removed providers into user DBs.
- 2026-09-16: `web/open-sse/config/*.js` entries are stripped (in-tree, contains hardcoded iFlow clientSecret) despite no `web/src` imports.
- 2026-09-16: `tests/provider_baseline.json` verified unreferenced repo-wide — removed-provider objects stripped as hygiene, no test updates for it.
- 2026-09-16: `thinkingLevels.ts` and hunyuan `capabilities.rs` lines stay — provider-agnostic thinking-format infrastructure.
- 2026-09-16: `nslale` does not exist in the repo; `nscale` is out of scope.
- 2026-09-16: Fixed commit order perplexity → qoder → iflow → gemini-cli → tail; Gemini CLI last as the riskiest (Antigravity adjacency).

## Checkpoint History

- 2026-09-16: Frozen R1-R6 from the approved audited plan. Next: R1 Perplexity Web removal.

## Completion

- Resolved outcomes: (pending)
- Commands and artifacts: (pending)
- Constraint and diff-scope check: (pending)
- Final status: Active.
