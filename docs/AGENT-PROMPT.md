# Задание coding agent: одна итерация OpenProxy lean plan

**Цель:** лёгкий provider proxy с минимальной добавленной задержкой и ограниченной RAM. OpenCode/мой harness владеет agent loop, историей, compaction, tools и временными retries. Прокси владеет credentials/OAuth, приватным account routing, необходимым protocol mapping, transport и ресурсными пределами. Не создавай второй harness внутри proxy.

## Прочитать до изменений

Прочитай актуальный repository `AGENTS.md`, затем `PLAN.md`, `CHECKPOINTS.json` и необходимые evidence sections из `AUDIT-EVIDENCE.md`. План проверен на архиве SHA-256 `e1613d03c34bc8a76fb8f1f5bd65c6074e727396144eca709ea9d834b6a3ffe3`. Мой checkout после итераций может быть новее: проверь symbols, текущий diff и callers; не перезаписывай мои изменения.

Выполни **только один следующий READY checkpoint**. На первом запуске — C00; затем первая поставка C01, C02, C03, C04, C05, C14, C15. Дальше следуй dependencies и приоритетам PLAN. Обнаруженный memory/security риск не откладывай за косметическим хвостом; оформи его отдельным dependency-consistent checkpoint.

## Обязательные границы

Удаляй ненужную работу до её оптимизации: Kiro semantic repair/history replay, глобальный Claude header replay, завершённые token-keyed caches и независимые generation retry loops. Не заменяй их новым общим cache framework. Сохрани HTTP connection reuse, корректную OAuth singleflight coordination, security state, достоверные model metadata и необходимый provider continuation protocol.

Не обрезай prompt/tools и не меняй присланную историю. Не ломай providers → Available Models → ModelSelectModal → OpenCode config/discovery; `opencode.source` и пользовательские custom/enabled/disabled models обязательны. `openproxy.v1.*` — additive-only: для удаления старой политики/настройки требуется описанная миграция/депрекация, а не новый смысл под старым полем. Административный UI не удаляется вместе с optional BasicChat.

Не добавляй sleep/retry после начала ответа; учти единый account/auth attempt budget. Не отключай TLS/SSRF/шифрование/auth/аудит молча. Лёгкий неблокирующий metadata logging имеет явные ограничения durability/overflow; lossless durable logging не обещает zero-wait.

## Порядок одной итерации

1. Определи checkpoint и dependencies; зафиксируй одну проверяемую цель и область. Сначала исследуй актуальные callers. Если задача содержит независимые изменения или слишком большой diff, выдели sub-checkpoints в tracker до реализации; за этот запуск выполни один.
2. Добавь воспроизводимый failing/characterization test, затем минимальную реализацию. Используй mock upstream и отдельную временную БД/config. Не вызывай реальные платные модели и не читай/переписывай production credentials ради теста.
3. Выполни targeted tests и затронутые regression gates. Сравни одинаковые release features, workload/concurrency и logging semantics; RAM не «улучшается» простым уменьшением числа успешных запросов.
4. Запиши фактические команды и результаты, semantic changes, измерения и rollback. Обнови ровно этот checkpoint. `done` допустим только после его обязательных gates. `not_applicable` — только для conditional, с доказательством; неизвестная реальная версия harness не означает совместимость проверена.
5. Проверь diff на секреты и несвязанные изменения. Не обновляй зависимости/lockfile, не переписывай весь chat.rs, не меняй allocator и runtime threads заодно. Остановись после отчёта одной итерации.

Отсутствие Cargo, upstream credentials или реального OpenCode binary отражай как конкретный blocked/unverified gate. Не выдумывай pass, latency/RSS и проценты. При доступной безопасной работе сделай её и перечисли проверенные/непроверенные части, не заменяй итерацию общим вопросом о подтверждении уже утверждённой цели.

## Формат отчёта

```text
Checkpoint: Cxx — название
Статус: done | blocked | not_applicable
Изменены: пути файлов
Результат: одна реализованная цель
Удалённое состояние/работа: что действительно исчезло
Контракт/миграция: что сохранилось и что изменено явно
Тесты: точные команды, pass/fail/skip
Измерения: baseline → после, workload/features; либо «не измерено»
Непроверенное: конкретные ограничения, без выдуманных гарантий
Риски и откат: конкретно
Следующий READY: Cyy
```

Для продолжения этой же задачи повторно используй этот prompt и актуальный tracker. **Не запускай реализацию всех checkpoints за один проход.**
