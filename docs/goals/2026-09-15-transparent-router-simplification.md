# Goal: Transparent router simplification

Status: complete
Source: User instruction on 2026-09-15 to remove request intervention, retained accounting data, model orchestration, non-chat services, and MITM; commit after each removal; then push and deploy.
Last updated: 2026-09-16

## Objective

Reduce OpenProxy to a transparent chat/tool-calling router: it may choose an explicitly configured route and translate wire formats, but it must not silently rewrite agent behavior, retain response/usage history, orchestrate model ensembles, host unrelated agent/media services, or intercept connections.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Commit each independently buildable subsystem removal before starting the next. Finish when every required outcome is resolved and affected constraints remain satisfied.

## Frozen Contract

### Required Outcomes

- R1: Remove the completed-response cache from the request path and administration surface.
  - Source: “Кэш готовых ответов — удалить полностью”.
  - Acceptance: `src/core/cache/`, `AppState.response_cache`, chat cache hit/fill behavior, cache administration routes/settings, and dependencies used only by this cache no longer exist.
  - Primary evidence: Repository search plus focused Rust build/tests for chat and server routing.
  - Status: verified
  - Evidence: Response-cache module/state/chat/admin/dashboard symbols are absent. `cargo clippy --all-targets --all-features`, 1,725 library tests, 12 `chat_completions` integration tests, and `pnpm run build` passed.

- R2: Remove server-owned prompt/history/content policy while preserving protocol translation.
  - Source: “Обрезание истории и серверную политику промптов — удалить” and “Прокси меняет модель и необходимое представление протокола. Содержание задачи и поведение агента определяет клиент.”
  - Acceptance: Combo history stripping/capacity adaptation, payload rules/system-prompt rewriting, guardrail prompt-injection/PII rewriting, synthetic bypass/naming replies, and provider thinking overrides are absent from chat handling.
  - Primary evidence: Repository search and focused chat/combo tests proving requests still reach normal dispatch.
  - Status: verified
  - Evidence: Payload/system-prompt rules, guardrails, synthetic bypass/naming responses, provider thinking overrides, capacity augmentation, history stripping, Claude cloaking, executor-owned branding/neutralization prompts, Antigravity competitive-prompt rewriting, and proxy-authored Kiro time/agentic prefixes are absent. Client system/developer messages, tool declarations, and explicit reasoning translation remain. Clippy and all 1,343 library tests passed.

- R3: Remove Codex behavioral defaults but retain required Codex wire compatibility.
  - Source: “выкинул ... DEFAULT_CODEX_INSTRUCTIONS” and separate necessary wire constraints from the default `reasoning.effort = "low"` policy.
  - Acceptance: Missing/empty client instructions do not acquire Codex CLI behavior, and the proxy does not invent a reasoning effort; tool calls, input items, tool schemas, images, and explicit client reasoning continue to translate.
  - Primary evidence: Focused Codex executor tests.
  - Status: verified
  - Evidence: The Codex CLI instruction constant and default `reasoning.effort = "low"` are absent. Missing instructions produce only the neutral required empty wire field; explicit reasoning and model suffixes still translate. Focused Codex tests (67), clippy, and all 1,680 library tests passed.

- R4: Remove internal usage history, pricing, aggregates, live usage, and exports from request processing.
  - Source: “Историю usage и расчёт стоимости — убрать из обработки запросов”.
  - Acceptance: Requests no longer append to or clone an in-memory/SQLite usage history and the pricing/dashboard live-usage/export surfaces are removed. Upstream usage remains in API responses. The existing bounded structured application-request record remains the sole request journal and contains request/route/provider/model/status/duration/token metadata without prompts, full responses, or secrets.
  - Primary evidence: Repository search plus focused chat/application-log and response-translation tests.
  - Status: verified
  - Evidence: Request-time usage history, in-memory/SQLite usage snapshots and writers, aggregate/live APIs and dashboard, usage/quota-counter CLI commands, budget enforcement, usage export/import, pricing engine/storage/API/UI/CLI, and cost/latency-based combo ordering are removed. The retained 30-day request journal emits only request/route/provider/model/status/duration/input/output token metadata. Final pricing checkpoint passed the dashboard build, clippy, 1,638 library tests, 53 focused combo tests, and 11 focused database tests.

- R5: Reduce combos to explicit ordered fallback routes.
  - Source: “Combo превратить в простой маршрут, а не оркестратор моделей”.
  - Acceptance: Fusion, hedging, shadow, auto-combo, price/speed/quality ordering, round-robin state, quarantine, and capacity adaptation are removed. A combo alias resolves only its explicitly configured ordered model/account routes and may fall back to the next explicit entry after failure.
  - Primary evidence: Focused combo/chat tests and repository search for removed strategies/modules.
  - Status: verified
  - Evidence: Combo dispatch now walks only enabled, explicitly configured members in declared order and stops at the first success. Fusion, hedging, shadow, auto-combo, cost/speed/quality ordering, combo round-robin state, quarantine/health API, capacity pre-gating, capability reordering, strategy settings, CLI flags, and dashboard controls are absent. Account-level fallback remains separate. Dashboard build, clippy, 1,594 library tests, 12 chat integration tests, and 7 focused combo tests passed.

- R6: Remove standalone media generation/transcription and embeddings while preserving images attached to chat messages.
  - Source: “Media, embeddings ... Под удаление идут `src/core/media/`, media/STT-модули API, маршруты embeddings, генерации изображений, аудио и видео, соответствующие CLI-команды и настройки” and “поддержку изображений внутри сообщений не вырезай”.
  - Acceptance: Standalone image/audio/video/STT/embedding routes, implementations, CLI commands, and settings are absent; multimodal image inputs in chat/Codex continue to compile and pass focused tests.
  - Primary evidence: Server/CLI route search plus focused multimodal chat/Codex tests.
  - Status: verified
  - Evidence: Removed `src/core/media/`, standalone media/STT/search APIs and routes, media CLI, custom embedding behavior, media-only provider registrations, dashboard media-provider pages/settings, and focused standalone tests. Chat stream collapsing moved under `core::chat`; chat/Codex image prefetch and protocol translation remain. Dashboard build, clippy, all 1,434 library tests, 65 focused Codex tests, 328 translator tests, 5 stream-flag tests, 99 affected integration tests, and OpenCode model discovery tests passed.

- R7: Remove built-in MCP, A2A, and evaluation services while preserving proxied tool calling.
  - Source: “Также я бы удалил: `src/core/mcp/`, `src/core/a2a.rs`, `src/core/eval/` ... Tool calling при этом остаётся.”
  - Acceptance: Those core modules, API routes, `a2a_task_store`, management interfaces, and CLI commands are absent; chat tool declarations/calls/results remain supported.
  - Primary evidence: Repository search plus focused tool-call translation tests.
  - Status: verified
  - Evidence: Deleted `src/core/mcp/`, `src/core/a2a.rs`, `src/core/eval/`, their HTTP routes, A2A task state, MCP bridge/native server, CoWork MCP marketplace/probe/configuration UI, and MCP tool-deduplication policy. CoWork model/endpoint configuration remains. Repository search found no retained service routes/modules; dashboard build, clippy, all 1,397 library tests, 328 focused translator tests, and 4 CoWork API tests passed.

- R8: Remove MITM and certificate interception infrastructure from ordinary HTTP clients.
  - Source: “удалил `src/core/mitm/`, MITM API/CLI, handle из AppState ... `ClientPool` uses `MitmBypassResolver`”.
  - Acceptance: MITM core/API/CLI/state and certificate dependencies are absent, and ordinary provider clients resolve explicitly configured URLs without an MITM-specific resolver.
  - Primary evidence: Repository search plus focused client/server build/tests.
  - Status: verified
  - Evidence: Deleted `src/core/mitm/`, MITM certificates/listener/capture/hosts logic, MITM API and CLI commands, Antigravity MITM management, `AppState` handle, settings/persistence fields, dashboard surfaces, and dedicated dependencies. `ClientPool` now uses reqwest's normal resolver for explicit provider URLs; only the shared private-address check remains in `core::dns`. Dashboard build, Rust check/clippy, all 1,358 library tests, and 16 focused CLI/database tests passed.

- R9: Deliver the completed simplification.
  - Source: “коммит делать после каждого выпиливания и потом пуш деплой”.
  - Acceptance: Every subsystem removal is an independently buildable Conventional Commit on `main`; the completed goal is pushed to `origin/main`, production is rebuilt through Docker Compose without deleting its volume, and `/health` succeeds.
  - Primary evidence: Git history/push output, `docker compose up -d --build`, `docker compose ps`, and `curl -fsS http://127.0.0.1:4623/health`.
  - Status: verified
  - Evidence: Fourteen independently buildable removal commits plus the initial goal commit were pushed to `origin/main` through `46d97b70`. `docker compose up -d --build` rebuilt and recreated `openproxy-prod-openproxy-1` without deleting its volume; Compose reports the service healthy and `curl -fsS http://127.0.0.1:4623/health` returned `status: ok`.

### Constraints

- C1: Preserve provider-native prompt caching fields, `prompt_cache_key`, cache-related usage, and upstream usage returned to clients.
- C2: Preserve images embedded in chat messages; only standalone media services are removed.
- C3: Preserve chat tool declarations, calls, and results; only proxy-hosted MCP/A2A/evaluation services are removed.
- C4: Preserve explicit client reasoning parameters and format/protocol normalization; remove only proxy-invented behavior policy.
- C5: Preserve provider/account configuration, explicit account fallback, Available Models, combos, OpenCode configuration, and `ModelSelectModal` consistency where not directly superseded by R5.
- C6: Preserve the frozen additive-only `openproxy.v1.*` envelope fields still exposed by retained resources; removed resources/routes/CLI commands are intentional breaking product removals.
- C7: Preserve the production `openproxy-prod-data` volume and secrets during deployment; never stage `.env.prod` or runtime data.

### Non-goals

- Replacing removed response caching, accounting, orchestration, media, agent-service, or MITM subsystems with alternate implementations.
- Removing Codex/tool/image protocol compatibility needed for transparent chat forwarding.
- Changing client-owned prompts, tool execution policy, or explicit reasoning choices.
- Optimizing or migrating historical usage/media/MITM data after their runtime surfaces are removed.
- Refactoring unrelated provider executors, dashboard pages, persistence, or authentication.

## Change Envelope

- Target: The directly referenced cache, chat policy, Codex policy, usage/pricing, combo, media/embeddings, MCP/A2A/eval, MITM/DNS runtime paths and their direct server, CLI, dashboard, persistence, tests, dependencies, and docs consumers.
- Expected paths, symbols, and direct consumers: `src/core/{cache,combo,guardrails,media,mcp,mitm,usage}`, `src/core/{a2a,eval,dns}.rs`, `src/core/executor/codex.rs`, `src/payload_rules.rs`, `src/server/api/`, `src/server/state.rs`, CLI definitions/dispatch, directly coupled DB repositories/schema fields, directly coupled dashboard pages/components, nearest tests, `Cargo.toml`, and architecture/user docs that advertise removed routes.
- Allowed artifacts: Deletions, minimal rewiring of retained chat/fallback paths, focused regression-test updates, dependency pruning, this goal document, and dashboard rebuild artifacts only if tracked by repository convention.
- Forbidden artifacts: Replacement services/caches/accounting stores, new dependencies, migrations whose only purpose is historical cleanup, unrelated refactors, secrets, local configuration, database files, or removal of chat images/tool calling/provider prompt caching.
- User or harness budget: No explicit LOC/time budget. Use one independently buildable commit per coherent subsystem removal and stop at the frozen finish line.

## Current Checkpoint

- Closes: R1-R9
- Smallest next action: None; closure passed.
- Expected evidence: Complete.
- Stop or replan if: A newer user instruction supersedes this completed objective.

## Current State

- Resolved: R1-R9.
- Last relevant evidence: Code was pushed through `46d97b70`; the production image rebuilt successfully, Compose reports `openproxy-prod-openproxy-1` healthy, and `/health` returned `status: ok`.
- Blocker: None.
- Next: None.

## Material Decisions

- 2026-09-15: Interpret “одну структурированную запись на запрос” as retaining the existing bounded SQLite application-request journal, not introducing a replacement usage subsystem.
- 2026-09-15: Retain only ordered, explicitly configured combo fallback. Automatic model selection and orchestration are removed.
- 2026-09-15: Treat the user's listed whole-subsystem removals as intentional API/CLI product removals; do not preserve compatibility shims for deleted services.
- 2026-09-15: Split delivery into atomic subsystem-removal commits, each verified before commit, as explicitly requested.
- 2026-09-16: Retain `/v1/web/fetch`: it is a separate URL-fetch utility, not one of the requested standalone media/STT/embeddings/image/audio/video services. Remove the hosted search path that depended on `src/core/media/`.
- 2026-09-16: Leave historical MITM KV rows/certificate files inert rather than destructively migrating user data; remove every runtime reader, writer, route, command, and export surface.

## Agreed Implementation Plan (source copy)

### 1. Что выкинуть первым: вмешательство в запросы и накопление данных

#### Кэш готовых ответов — удалить полностью

Конкретно: `src/core/cache/`, поле `AppState.response_cache`, функции `response_cache_hit()` и `cache_miss_response()` в `src/server/api/chat.rs`, связанные административные маршруты.

Это не абстрактная рекомендация «кэширование сложно». В текущем коде:

- TTL по умолчанию — 86 400 секунд, то есть сутки;
- ограничение — 10 000 записей, а не общий объём памяти;
- `cache_miss_response()` собирает тело ответа целиком с пределом 64 MiB, затем делает `bytes.to_vec()` для хранения;
- проверка и заполнение кэша встроены в обычную обработку non-streaming запросов, с исключением для соответствующих Codex web-search запросов.

Для твоего сценария это ненужная скрытая семантика: повторный запрос может вообще не попасть к провайдеру. Плюс долго живущие тела ответов и дополнительные копирования.

Это не доказанная утечка памяти. Это явно существующее удержание данных, которое тебе не нужно. Здесь я бы не улучшал eviction, не добавлял настройки и не переписывал на другой cache crate. Просто удалил подсистему.

При этом не путать этот кэш с нативным prompt caching провайдера. Передачу клиентских `prompt_cache_key`, cache-related полей и соответствующего usage ломать не нужно. Например, Codex-адаптер отдельно обрабатывает `prompt_cache_key`; это другая функция.

#### Обрезание истории и серверную политику промптов — удалить

Самый прямой кандидат — `src/core/combo/capacity_adapter.rs`.

Он не только подбирает дополнительные модели по capabilities. Для добавленных моделей предусмотрен `strip_history_for_context()`: история обрезается под контекстное окно с эвристикой бюджета. В `chat.rs` capacity adapter действительно подключён к combo-маршрутизации. Это ровно та ответственность, которую ты хочешь оставить клиенту.

Заодно я бы убрал:

- `src/payload_rules.rs` и его вызовы. Сейчас в `chat.rs` применяются `apply_system_prompt()` и `apply_request_rules()` ещё до разделения на direct/combo dispatch. Для твоей архитектуры роутер не должен незаметно переписывать системный промпт или произвольные поля запроса.
- `src/core/guardrails/`. Здесь находятся regex-проверки prompt injection и маскирование персональных данных в JSON. Для прозрачного личного роутера это чужая ответственность. Маскирование строк потенциально меняет и полезные данные агента: например, адреса в коде или тестовых примерах. Проверки безопасности выполнения инструментов должны оставаться у харнесса, а не превращаться в замену строк внутри прокси.
- Bypass/naming-эвристики и серверные thinking overrides. В `chat.rs` есть выдача синтетического ответа вместо обращения к модели, отдельная обработка naming-запросов и `inject_provider_thinking()`. Я бы оставил передачу явно запрошенных reasoning-параметров, но убрал решения прокси о том, что клиенту «на самом деле нужно».

Правило здесь простое:

> Прокси меняет модель и необходимое представление протокола. Содержание задачи и поведение агента определяет клиент.

#### Особенно обратил бы внимание на зашитый промпт Codex

В `src/core/executor/codex.rs` есть большой `DEFAULT_CODEX_INSTRUCTIONS` — фактически инструкции поведения Codex CLI, включая работу с файлами, планирование и формат ответов.

Он подставляется, когда `instructions` отсутствует или является пустой строкой:

```rust
.get("instructions")
.and_then(Value::as_str)
.filter(|s| !s.is_empty())
.unwrap_or(Self::DEFAULT_CODEX_INSTRUCTIONS);
```

Это не только константа для документации — fallback действительно применяется при формировании запроса.

Вот это я бы выкинул без сентиментальности. Твой личный агент не обязан внезапно получать поведение Codex CLI только потому, что запрос отправлен через Codex-провайдера.

Если upstream требует поле `instructions`, адаптер должен обеспечить валидный запрос, но не добавлять огромную чужую поведенческую инструкцию. Аналогично стоит отделить необходимые wire-ограничения от текущего выбора `reasoning.effort = "low"` по умолчанию.

Сам `codex.rs` целиком удалять нельзя: рядом находится полезная нормализация tool calls, input items и схем инструментов. Резать нужно политику, а не совместимость.

#### Историю usage и расчёт стоимости — убрать из обработки запросов

Здесь есть ещё более предметная причина, чем «мне не нужны графики».

В `src/db/mod.rs::update_usage()` при обновлении выполняются:

```rust
let prev = (*self.usage_snapshot()).clone();
let mut next = prev.clone();
```

То есть копируется `UsageDb`, затем делается ещё одна копия. В `src/core/usage/tracker.rs` перед добавлением записи также выполняется поиск дубликата через `db.history.iter().any(...)`.

Получается, стоимость записи usage связана с уже накопленной историей. При этом сама история загружается в in-memory snapshot из SQLite.

Для твоего проекта я бы не оптимизировал эту бухгалтерию, а убрал её: историю запросов, pricing engine, dashboard-агрегации, live usage и связанные экспорты.

Вместо этого оставил бы одну структурированную запись на запрос:

```text
request_id, route, provider, model, status,
duration_ms, input_tokens, output_tokens
```

Без промптов, полных ответов и секретов. При необходимости — несколько агрегированных счётчиков. Передачу upstream usage клиенту при этом сохранить: удаление собственной истории не означает удаление статистики из API-ответов.

### 2. Combo превратить в простой маршрут, а не оркестратор моделей

В `src/core/combo/mod.rs` сейчас есть стратегии `Fallback`, `RoundRobin`, `Fusion`, `AutoCombo`, `Hedging`, `Shadow`, `Cheapest`, `Fastest`, `Quality`, плюс карантин и состояние ротации.

Для твоих задач я бы убрал fusion, hedging, shadow, auto-combo, выбор по цене/скорости/«качеству» и capacity adapter.

Конкретные модули-кандидаты внутри `src/core/combo/`:

- `auto_combo`
- `fusion`
- `hedging`
- `shadow`
- `ordering`
- `capacity_adapter`

Это существующие модули, а не предполагаемые названия.

Вместо combo как платформы достаточно:

```text
alias → credential + upstream_model + protocol
```

Например: `work` отправляется в конкретный Codex credential, `hobby` — в конкретный маршрут OpenCode Go, `glm` — в z.ai.

Простой fallback можно оставить только как явно заданный список, если он действительно нужен. Не нужно сохранять девять стратегий ради возможного использования одной.

И я бы разделил два разных механизма: выбор ключа для того же провайдера и переключение на другую модель/провайдера. Второе меняет поведение агента и должно быть сознательной настройкой, а не автоматическим «спасением» запроса.

### 3. Что удалить целыми подсистемами

#### Media, embeddings и встроенные агентные сервисы

Под удаление идут `src/core/media/`, media/STT-модули API, маршруты embeddings, генерации изображений, аудио и видео, соответствующие CLI-команды и настройки. Эти маршруты сейчас действительно зарегистрированы в сервере.

Но поддержку изображений внутри сообщений не вырезай вместе с image generation. Прикрепить скриншот к запросу coding-агента и вызвать отдельный сервис генерации изображений — разные возможности. В chat/Codex-пути уже есть отдельная обработка входных изображений.

Также я бы удалил:

- `src/core/mcp/`
- `src/core/a2a.rs`
- `src/core/eval/`

И связанные MCP/A2A API, `a2a_task_store` в `AppState`, интерфейсы и команды управления. Эти подсистемы присутствуют отдельно от базовой пересылки LLM-запросов.

Tool calling при этом остаётся. Прокси должен передавать объявления инструментов, вызовы и результаты. Но сам становиться MCP-сервером, исполнять инструменты или управлять жизненным циклом агентных задач ему незачем.

#### MITM и специальную инфраструктуру перехвата

Я бы удалил `src/core/mitm/`, MITM API/CLI, handle из `AppState` и связанную инфраструктуру сертификатов.

Отдельно пересмотрел бы `src/core/dns/`: текущий `ClientPool` использует `MitmBypassResolver`, то есть тема MITM проникла даже в создание обычного HTTP-клиента. Это не просто выключенный пункт панели.

Для личных харнессов я бы исходил из явной настройки `base_url`, без перехвата чужих соединений.

## Checkpoint History

- 2026-09-16: R9 and closure passed. All R1-R8 evidence is current, the 14 removal commits and initial goal commit were pushed to `origin/main`, production was rebuilt and recreated with Docker Compose while preserving its volume, Compose reports healthy, and `/health` returned `status: ok`. Goal complete.
- 2026-09-16: R2 follow-up passed. Removed Claude OAuth prompt/tool cloaking, Antigravity competitive-brand rewriting and default prompt injection, OpenAI-to-Claude default branding, CodeBuddy prompt replacement, MiMo marker injection, Kiro current-time/agentic prefixes, and stale Codex prompt files while retaining wire translation and explicit reasoning. Format/diff checks, clippy, and all 1,343 library tests passed. Next is R9 closure/push/deploy.
- 2026-09-16: R8 passed. Removed MITM certificates/listener/capture/hosts infrastructure, API/CLI/state/settings/dashboard surfaces, certificate dependencies, and the MITM-specific DNS resolver while retaining SSRF private-address checks. Dashboard build, Rust check/clippy, 1,358 library tests, and 16 focused CLI/database tests passed. Next is the deferred R2 policy follow-up.
- 2026-09-16: R7 passed. Removed built-in MCP bridge/server, A2A lifecycle and state, evaluation library, related routes and CoWork MCP management UI/configuration, plus tool-declaration deduplication policy. CoWork endpoint/model configuration and protocol-level tool translation remain. Dashboard build, clippy, 1,397 library tests, 328 translator tests, and 4 CoWork API tests passed. Next is R8 MITM/DNS removal.
- 2026-09-16: R6 passed. Removed standalone media/STT/embeddings/image/audio/video/search implementations, routes, CLI, provider/dashboard configuration, and tests; moved retained SSE-to-JSON chat handling under `core::chat`. Chat image input translation and Codex image prefetch remain. Dashboard build, clippy, 1,434 library tests, focused multimodal tests, affected integration tests, and OpenCode model tests passed. Next is R7 MCP/A2A/evaluation removal.
- 2026-09-15: R4 partial — removed internal request usage history, live aggregation, usage dashboard/CLI/export, and budget accounting; retained a 30-day metadata-only request journal and provider-native quota fetching. Dashboard build, clippy, 1,657 library tests, request-log integration, and 12 chat integration tests passed. Next: remove pricing and its combo ordering consumers.

- 2026-09-15: Contract frozen from the user-supplied plan. The source plan is copied above. First implementation checkpoint is the completed-response cache removal.
- 2026-09-15: R1 passed. Removed the response cache, chat hit/fill path, state, admin stats route, dashboard card, focused cache test, and cache-only direct dependency. Provider-native prompt-cache translation remains untouched. Next is R2 request-policy removal.
- 2026-09-15: R2 checkpoint: removed payload rules and system-prompt overrides from chat, settings, API, and dashboard. Clippy, 1,713 library tests, and the dashboard build passed. Guardrails and the remaining request-policy hooks are next.
- 2026-09-15: R2 checkpoint: removed the unused guardrail registry and its prompt-injection/PII mutation implementation. Clippy and 1,694 library tests passed. Synthetic bypass and thinking policy remain.
- 2026-09-15: R2 checkpoint: removed Claude request bypass/naming heuristics, synthetic responses, the stale setting, and its CLI-tool toggle. Clippy, 1,687 library tests, and the dashboard build passed. Thinking and capacity policy remain.
- 2026-09-15: R2 checkpoint: removed persisted `providerThinking` policy and source-body injection. The provider-page selector remains client-explicit by only appending a reasoning suffix to copied model IDs. Clippy, 1,687 library tests, and the dashboard build passed. Capacity/history adaptation remains.
- 2026-09-15: R2 passed: removed capacity-adapter model injection, history stripping, and the stale setting while retaining explicit combo members and context-limit rejection. Clippy, 1,680 library tests, and 12 chat integration tests passed. Next is Codex policy.

## Completion

- Resolved outcomes: R1-R9 verified.
- Commands and artifacts: Dashboard builds passed for affected frontend checkpoints; final Rust gate passed `cargo fmt --check`, `git diff --check`, `cargo clippy --all-targets --all-features`, and 1,343 library tests. `git push origin main`, `docker compose up -d --build`, `docker compose ps`, and `curl -fsS http://127.0.0.1:4623/health` succeeded.
- Constraint and diff-scope check: Provider-native prompt caching/upstream usage, chat images, tool calling, explicit reasoning, provider/account configuration, ordered fallback, Available Models/OpenCode integration, stable retained envelopes, secrets, and the production data volume remain preserved. Removed product routes are intentional per the frozen contract.
- Final status: complete
