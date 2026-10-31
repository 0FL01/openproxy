# Goal: Claude OAuth REPLACE (relay-parity mimicry, C46)

Status: unmet
Source: user instruction 2026-09-25 (REPLACE текущей Claude OAuth-логики на relay-parity; мимикрия Claude Code любой ценой; cache hit корректен; исключение для claude-провайдера) + Evidence-ресёрч 2026-09-25 (claude-relay-service / CCR v3 / opencode-плагины) + параллельный аудит 6 ревьюеров 2026-09-25
Last updated: 2026-10-31

Superseded by the user's 2026-10-31 native-Claude hard cut. The OAuth/token-import/mimicry finish line is now explicitly forbidden; the historical requirements below are not executable work. The active replacement is [Claude Agent native subscription](2026-10-31-claude-agent-native.md).

## Objective
Claude-подписка (Pro/Max) подключается через OpenProxy по OAuth и работает из OpenCode через существующий `plugins/openproxy-models.js` без его изменений: живой PKCE-логин, живой refresh, wire-мимикрия Claude Code на транспорте, модели подписки в `/v1/models` с лимитами.

## Execution Directive
Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes
- R1: Исключение C46 оформлено в контракте
  - Source: user 2026-09-25 — «для claude провайдера делаем исключение», mimicry обязательна иначе «смысла добавлять claude нету».
  - Acceptance: `contracts/lean-proxy.md` + `contracts/lean-proxy.json` (`registered_request_transforms`, `passthrough_policy`) + `docs/CHECKPOINTS.json` содержат C46 carve-out: только first-party `claude`/`anthropic` + `auth_type == "oauth"` + таргет `api.anthropic.com`; остальное без изменений.
  - Primary evidence: `git diff --stat` трёх файлов + `cargo test -p openproxy --lib parity_tests` зелёный.
  - Status: verified
  - Evidence: contracts/lean-proxy.md (C46 row), contracts/lean-proxy.json (passthrough_policy flag + claude_oauth_harness_identity), docs/CHECKPOINTS.json (C46 active); parity_tests 11 passed 2026-09-25.
- R2: OAuth-константы = значения relay, единый источник
  - Source: user 2026-09-25 — «повторять логику claude-relay-service»; аудит А1 (консолидация, env-override везде).
  - Acceptance: один модуль (`src/oauth/providers.rs` — каноника; копии в `oauth.rs`, `token_refresh.rs`, `provider_connection_test.rs` удалены): `client_id 9d1c250a…`, `token_url https://platform.claude.com/v1/oauth/token`, `redirect_uri https://platform.claude.com/oauth/code/callback` + `code=true`, scopes набора relay с `user:sessions:claude_code`; `OPENPROXY_CLAUDE_*` применяется везде (включая refresh и generic-start).
  - Primary evidence: `rg "api.anthropic.com/v1/oauth/token|9d1c250a" src/ --count` показывает только канонический модуль; lib-тесты URL-строителей зелёные.
  - Status: verified
  - Evidence: lib 984 passed; oauth_auth_code_compat_api 5 passed; c09 2 passed; c04 3 passed; fmt+clippy clean 2026-09-25.
- R3: Login REPLACE — один путь
  - Source: аудит А2 (принят): Путь 1 для claude неремонтопригоден (client_id `openproxy`, form-обмен); оставить Путь 2.
  - Acceptance: `claude` исключён из `start_oauth_flow`/`oauth_callback` с явным `unsupported_flow`; сохранение через `create_imported_oauth_connection` + profile-подтяжка (`user:profile` scope) для дедупликации; `OAuthModal.tsx` разбирает голый `code#state` для claude.
  - Primary evidence: unit-тест `code#state`-парсинга + ручной проход authorize→paste→exchange на dev.
  - Status: verified
  - Evidence: lib 984 passed; oauth_auth_code_compat_api 5 passed (email-упсерт, no-dup, token-update); fmt+clippy clean 2026-09-25. Живой проход — в R8.
- R4: Harness transform relay 1:1 за гейтом
  - Source: user 2026-09-25 + Evidence п.1в; точка внедрения — финализация Messages-body в claude-адаптере (`prepare_claude_request`, `claude_format.rs`).
  - Acceptance: гейт (provider ∈ {claude,anthropic} + oauth + first-party target); классификатор (UA `claude-cli` + similarity → passthrough, иначе spoof, чистая функция); spoof (reference system константа с версией; релокация оригинала в user/assistant-пару; `metadata.user_id` device-per-connection + session-per-request; strip `x-anthropic-billing-header`); заголовки (Bearer, beta с `oauth-2025-04-20` + `claude-code-20250219`, `User-Agent: claude-cli/<pin>`, `x-app: cli`, клиентские не затирать).
  - Primary evidence: Rust-тесты классификатора/гейта/релокейшена + `cargo test -p openproxy --lib`.
  - Status: verified
  - Evidence: lib 991 passed; c04/c09/oauth integration green; fmt+clippy clean 2026-09-25. Коррекции: device_id фиксированный (как у relay) — тело аккаунто-независимо, re-prepare не нужен; first-party исключает gateway-ноды (dual-auth intact); first-party apikey → x-api-key (было Bearer).
- R5: Cache hit корректен
  - Source: user 2026-09-25 — «важно чтобы cache hit работал корректно».
  - Acceptance: reference system байт-стабилен; клиентские `cache_control` переезжают с контентом 1:1; синтетический ack без маркеров; `prompt_cache_key` passthrough; брейкпоинт на последнем reference-блоке детерминирован; классификатор не флиппует.
  - Primary evidence: тесты (байт-идентичность префикса, релокация маркеров, отсутствие маркеров на ack, passthrough настоящего claude-cli) + мок hit/miss на повторный идентичный запрос.
  - Status: verified
  - Evidence: 6 harness-тестов + детерминизм-тест; коррекция: маркеры НЕ релоцируются — `prepare_claude_request` владеет брейкпоинтами (strip + детерминированный re-add), хиты за счёт байт-стабильного префикса (reference system = 1 блок) и стабильного брейкпоинта на нём; ack без маркеров; `prompt_cache_key` не тронут; system массивом (не строкой как у relay) — wire-эквивалентно, нужно для брейкпоинта prepare.
- R6: Refresh-диагностика
  - Source: аудит А4 (принят): ядро refresh надёжно (singleflight, ротация атомарна), не хватает диагностики.
  - Acceptance: permanent-отказ (`invalid_grant` и соседи) персистится в `last_error`/`error_code` + текст «re-authorize» в quota/dashboard; quota 401 vs 403 разделены (force-refresh только по 401); alias `anthropic` в quota-ветке; мёртвый `refresh_provider_connection_credentials` удалён.
  - Primary evidence: unit-тесты permanent-отказа и сплита 401/403.
  - Status: verified
  - Evidence: token_refresh classification test; c23 quota split test; lib 992 passed; fmt+clippy clean 2026-09-25.
- R7: Discovery → OpenCode без изменений плагина
  - Source: user 2026-09-25 — использование через `@plugins/openproxy-models.js`; аудит А5 (принят).
  - Acceptance: `v1_models.rs` union static+discovered для claude (образец ветки codex); overlay limits/capabilities из `capabilities.rs` (пара context+output выживает в плагине); discovery-auth по `auth_type` (api_key→x-api-key, oauth→Bearer+beta).
  - Primary evidence: новые Rust-тесты (`source=="claude"`, `limit{context,output}`, union) + `node --test tests/opencode_models*.test.mjs` без изменений + ручной `curl /v1/models → cc/*` с лимитами.
  - Status: pending
  - Evidence: overlay+auth по auth_type в коде; cc static extended (4-8, 5-5) с exact capabilities; lib 993 + plugin tests green 2026-09-25; ручной curl ещё не делан.
- R8: Честность + gates + живая проверка
  - Source: аудит А6 (принят, 1 строка) + AGENTS.md workflow.
  - Acceptance: `notice.text` у `claude` в `providers.ts:81` (ToS/бан-риск, безопасный путь — API-ключ); `cargo test -p openproxy --lib`, clippy/fmt, `pnpm --dir web run build` зелёные; end-to-end на реальном Claude-аккаунте (authorize→paste→exchange→refresh→generation→`/v1/models`).
  - Primary evidence: зелёные gates + ручной E2E-чеклист с реальным аккаунтом.
  - Status: pending
  - Evidence:

### Constraints
- C1: `contracts/lean-proxy.md` frozen — меняется только добавлением C46 (R1); остальное не трогать.
- C2: ToS/бан-риск Anthropic принят владельцем сознательно; API-key путь (`x-api-key`) не трогаем и не спуфим.
- C3: `plugins/openproxy-models.js` не изменяется.
- C4: Живая E2E-проверка требует реального Claude-аккаунта (внешняя зависимость, не мокается).

### Non-goals
- Переименование AGENTS.md→CLAUDE.md (вредно, отклонено аудитом А6).
- Fallback между token-доменами; расширение scopes вне relay-набора; `metadata.user_id` вне гейта; curated static fallback-каталог; урезание beta-флагов; auth на compat-эндпоинтах; отдельный гайд по подключению; подмена system вне claude+oauth гейта.

## Change Envelope
- Target: REPLACE Claude OAuth: константы, login-путь, harness transform + cache-правила, refresh-диагностика, discovery-overlay, 1 строка UI-нотиса, контрактная запись C46.
- Expected paths, symbols, and direct consumers: `src/oauth/providers.rs` (`claude()`), `src/oauth/token_refresh.rs` (`refresh_claude_oauth_token`, `dispatch_oauth_refresh`), `src/server/api/oauth.rs` (`authorize_oauth_compat`, `exchange_claude_compat`, `build_claude_auth_url`, удаление claude из `start_oauth_flow`/`oauth_callback`), `src/core/translator/request/claude_format.rs` (`prepare_claude_request`), `src/core/executor/default.rs` (claude-ветки, `select_anthropic_beta`), `src/server/api/provider_models.rs` (`fetch_anthropic_models`), `src/server/api/v1_models.rs` (`build_models_list`), `src/server/api/usage.rs`, `src/core/usage/quota_fetcher.rs`, `src/server/api/quota_auto_ping.rs`, `web/src/shared/components/OAuthModal.tsx`, `web/src/shared/constants/providers.ts`, `contracts/lean-proxy.md`, `contracts/lean-proxy.json`, `docs/CHECKPOINTS.json`. Consumers: dashboard OAuthModal, OpenCode через `/v1/models` + `ludka2`.
- Allowed and forbidden artifacts: allowed — Rust + тесты, 1 строка TS-нотиса, контрактные записи; forbidden — изменения плагина, новые сервисы/очереди/миграции/публичные API, system-спуф вне гейта, переименование AGENTS.md.
- User or harness budget: живая проверка — только реальный аккаунт пользователя; без него E2E остаётся открытым пунктом R8.

## Current Checkpoint
- Closes: R1
- Smallest next action: внести C46 (строка таблицы в `lean-proxy.md` + поле в `lean-proxy.json` + запись в `CHECKPOINTS.json`, формулировка из R1) и прогнать `cargo test -p openproxy --lib parity_tests`.
- Expected evidence: diff трёх файлов + зелёный parity_tests.
- Stop or replan if: контрактная запись требует перенумерации/переформатирования манифеста сверх одной записи — тогда сначала согласовать формат.

## Current State
- Resolved: план заморожен (RECON + Evidence + аудит 6 ревьюеров + решение владельца о C46); goal-документ создан.
- Last relevant evidence: аудит 2026-09-25 (6/6 completed); C09 процитирован дословно — запрет касается семантики тела, wire-guards разрешены.
- Blocker: нет (реальная E2E-проверка понадобится только на R8).
- Next: R1 (C46), затем R2→R3→R4→R5→R6→R7→R8.

## Material Decisions
- 2026-09-25: relay-parity обязательна, иначе подписочный Claude бессмысленен (бан/блок) — решение владельца.
- 2026-09-25: исключение C46 только для first-party claude+oauth; API-key и сторонние ноды не спуфим.
- 2026-09-25: отклонено аудитом — смена token-домена вне relay-значения, fallback доменов, расширение scopes, `metadata.user_id` вне гейта, static fallback-каталог, trim beta, AGENTS.md→CLAUDE.md.
- 2026-09-25: ToS/бан-риск принят владельцем; закрывается честностью (R8 notice), не техникой.

## Checkpoint History
- 2026-09-25: goal создан из утверждённого плана (R1–R8), статус active; реализация не начата.
- 2026-09-26: R1–R7 реализованы и задеплоены (коммит b89f3bb8); Opus 5.5 добавлен (cc/claude-opus-5-5 + cc/claude-opus-5.5); R8 открыт (notice.text + живая E2E). Новый цикл: F1–F3 (ниже), F4 выкинут из scope решением владельца.

## Cycle 2: F1–F3 (frozen 2026-09-26, источник: binary-RECON 2.1.282 + аудит 4 ревьюеров)
- F1. Policy-guard (S): единый предикат is_claude_oauth_policy_rejection на 3 текста (401 'OAuth authentication is currently not supported'; 400/403 'this organization has been disabled' / 'OAuth authentication is currently not allowed for this organization'; 400 'only authorized for use with claude code' / 'cannot be used for other api requests') × 400|401|403 по сырому body (lowercase-contains) → skip-refresh до is_refreshable_auth_failure:1804; carve-out из 400-early-return chat.rs:1688; литерал AUTH_FAILURE (C43 чист); amend C13 1-2 строки (md+json). НЕ трогать: token_refresh.rs, codex_search.rs, тексты клиенту (H23 verbatim). Тесты: матрица текстов + no-refresh + fallback.
- F2. Beta (S): дроп token-efficient-tools-2026-03-28 из ANTHROPIC_BETA_BASE + синк quota_auto_ping.rs:70 (порядок: дроп → ассёрты → пинг); НЕ добавлять fine-grained (нет в CLI); НЕ делать haiku-ветку; мерж клиентских бет union+дедуп+bound 64 флага/4096Б, ours-first, санитизация ^[a-z0-9-]{1,64}$, fallback ours-only; ветку расширить на provider="anthropic" узко (first-party гард как 876-883, gateway-стрип не трогать); дописать 2 негативных ассёрта (нет token-efficient, нет fine-grained). Тесты строк opus/sonnet/haiku + мерж + bound.
- F3. Диагностика (XS): policy-отказ пишет last_error/error_code существующими полями (reuse предиката F1), без новых колонок/флагов/подавления роутинга.
- F4 DROPPED из scope (решение владельца 2026-09-26).
- Deferred: P3 tool-rename (fence: живой сэмпл + C47 + roundtrip-тесты). Dropped окончательно: billing-стрип, trim 9→4, context-1m-флаг, скопы mcp/file_upload, transport-switch, dual-send, blocked-флаг, sed-rename, title-кейс.
- 2026-09-26: F1–F3 реализованы, gates зелёные (lib 998, clippy, oauth/c04/c09/c23, parity 11/11, executor_pool 38/38, plugin 5/0), задеплоены (коммит 12d138c1).

## Completion
- Resolved outcomes:
- Commands and artifacts:
- Constraint and diff-scope check:
- Final status:
