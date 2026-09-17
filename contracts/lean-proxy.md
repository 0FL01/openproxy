# Lean proxy contract

This is the frozen C00 boundary for the [lean plan](../docs/CHECKPOINTS.json). The machine-checkable inventory is [`lean-proxy.json`](lean-proxy.json); implementation registries remain authoritative for exact runtime dispatch.

## Ownership

| Concern | Sole owner |
|---|---|
| Conversation history, compaction, tool execution, semantic repair, and temporal generation retries | OpenCode or the calling harness |
| Private credentials/OAuth, configured account/model routing, required protocol mapping, HTTP transport reuse, and resource/security limits | OpenProxy |

OpenProxy must not truncate, restore, or semantically reinterpret client prompt/history/tools. It may replace the routed model identifier, add provider-required wire fields and headers, translate protocols, refresh credentials, and try an eligible configured account within one bounded request-scoped attempt budget.

## Supported surface

The route and format-pair allowlists are in `lean-proxy.json`. Their source registries are:

- routes: `src/server/api/mod.rs`;
- provider target formats and translation pairs: `src/core/translator/registry.rs`;
- provider-specific executor dispatch: `src/server/api/chat.rs::forward_with_provider_fallback`;
- model metadata/discovery: `src/server/api/v1_models.rs::build_models_list`.

Same-format transport may be minimal, but byte-for-byte identity is not promised when model routing or a documented adapter requirement changes the body. Native streaming must not become collection merely to share an implementation.

C09 makes that minimal path a protocol decision: `source_format == target_format`
after dynamic model metadata is applied. Recognized, unknown, and absent client
identities therefore receive the same request semantics. Client detection remains
only for genuine transport preferences such as DeepSeek TUI streaming. OpenProxy
still replaces the routed model and applies documented native adapter wire guards,
but it does not autonomously re-anchor cache markers. Native Messages input reaches
the planner unchanged; incompatible protocol pairs use the registered translator.

## Product and security invariants

The following chain is one contract and must remain consistent:

`ProviderDetailPageClient/useAvailableModels` → `buildAvailableModels` → `ModelSelectModal` → OpenCode tool configuration → `/v1/models` → `plugins/openproxy-models.js`.

It preserves user custom/enabled/disabled models and canonical `opencode.source`. It also preserves TLS, SSRF protection tied to the actual connection, encrypted credentials, API/dashboard authentication, bounded audit behavior, provider-native cache fields/usage, required continuation state, and HTTP connection pooling.

The `openproxy.v1.*` namespace remains additive-only. Retired behavior is not assigned a new meaning under an existing field.

## Legacy data and migration

Persisted types intentionally preserve unknown fields through `extra` and `providerSpecificData`. Lean migrations therefore obey these rules:

1. Do not delete legacy values while loading or normalizing a database. Deprecated values remain exportable and round-trip unchanged unless a later checkpoint declares a separate data migration.
2. Stop consulting retired policy fields on the hot path, but do not silently reinterpret them. A rollback may re-enable the old reader without reconstructing user data.
3. In-memory replay/cache state is deleted, not moved to SQLite or replaced by another TTL/LRU cache.
4. Diagnostic lock/cooldown/health fields never suppress routing. Explicit user actions may clear them; success does not perform synchronous configuration housekeeping.
5. Configuration/credentials remain durable. Metadata logging may be lossy only in an explicitly selected lean mode with bounded event/byte queues and visible drop accounting.

| State | Transition contract |
|---|---|
| `providerSpecificData.kiroToolCallRepair` | C03 stopped semantic repair and second generation for all values. The stored key/value remains exportable and round-trips unchanged; its first runtime encounter per process emits a deprecation warning and has no effect on generation. |
| Process-wide Claude header cache | C04 deleted it without migration. Claude/Anthropic adapters now receive only an explicit identity/protocol allowlist from the current request; authorization, cookies, forwarding headers, and arbitrary extensions cannot enter that channel. Missing required protocol headers come from adapter defaults. |
| Kiro session-start replay | C05 deleted the process-wide `session_start`/system-prompt store without migration or replacement. Kiro history, system/thinking prefix, tools, and current content are now derived only from the current request; request-scoped protocol identifiers remain pending C06. |
| Session/continuation maps | C06 deleted both process-wide maps without migration. Client identifiers win; Antigravity preserves its UUID-plus-digits wire shape, while OpenCode fallbacks and identified Kiro continuation UUIDs are derived statelessly with adapter + configured-connection namespaces. Anonymous Kiro requests remain one-shot. Kiro binds continuation to the selected connection before transport. Retained entries/bytes are zero; dashboard auth `AppState.sessions` is untouched. |
| `settings.providerContextLimits` | C07 separated advertised client metadata from policy; C08 deleted the default chat-path estimator, synthetic 413, and Codex input-headroom rejection without changing that metadata. Existing values, the legacy `opencode` → `opencode-zen` alias, and the current missing/empty-map meaning (four 500,000 defaults, not “off”) remain unchanged and round-trip. Codex's 500,000/450,000/128,000 values are compatibility metadata, not proof of an upstream overdrive limit. Context/history/compaction policy belongs to the client; proxy memory safety uses independent byte bounds. Explicit count-tokens remains a separate API, and real upstream context errors retain their status/body. Rollback may add an explicitly announced compatibility reader over the preserved values, never a silent automatic fallback. |
| Client-identity passthrough and Claude cache re-anchoring | C09 deleted the User-Agent/provider passthrough gate and standalone cache-anchor rewrite. Same-format selection is derived from protocol capability after dynamic model metadata; client `cache_control`, `prompt_cache_key`, unknown fields, tools, reasoning, and provider extensions stay request-owned except for routed-model replacement and documented adapter wire guards. |
| DefaultExecutor generation retries | C10 deleted same-URL temporal retries and fixed sleeps for 429/502/503/504. Those raw responses retain status, body, and `Retry-After` for the request-scoped account planner. Configured provider endpoints and pooled transports remain; C13 subsequently removed its transitional executor-local 401/403 refresh attempt. |
| Codex SSE generation retries | C11 deleted Codex's three-attempt transient-error loop, fixed two-second sleeps, and 256 KiB user-output retry window. A successful SSE response now inspects at most 64 KiB while waiting only for its first complete event: recognized structured overload/rate-limit failures become an error for the request-scoped planner before commitment, while a normal first event is replayed immediately into the live stream. Errors after that event never start another generation. Original error event bytes, headers, configured endpoint, and pooled transport are preserved. |
| Antigravity generation retries | C12 deleted Antigravity's three-attempt temporal retry loop, Retry-After sleeps, jitter, and error-body keyword scheduling. Each configured endpoint receives one generation request; every HTTP response keeps its original status, headers, and live body for the request-scoped planner. Configured provider-node endpoints and pooled transport are preserved. Project discovery and onboarding remain separate C21-C22 work. |
| Account fallback and credential recovery | C13 makes the request-scoped chat planner the sole generation-attempt and 401/403 recovery owner. The upper bound is eligible configured accounts multiplied by bounded protocol endpoint surfaces, plus one globally permitted post-refresh generation attempt. Each normal account is selected once, 400/422 is terminal without account fan-out, and 429/5xx/transport failures may advance only to another eligible account without sleep. DefaultExecutor and Mimo no longer repeat generation or refresh internally; Kiro's three static endpoint surfaces consume the same shared budget. Raw final status/body/`Retry-After`, cancellation, pooled transports, inert cooldown data, and the prohibition on retry after downstream commitment remain. |
| `modelLock_*`, cooldown/error, and health fields | C14 removed generation and web-fetch success-path cleanup. Stored values remain historical/legacy diagnostics, not a claim about current route eligibility; they do not select accounts. The explicit `clearCooldown` action remains the narrow state transition. C24 separately removes default probes. |
| OAuth refresh coordination | C16 prepared an in-flight-only coordinator keyed by configured provider/connection identity plus a SHA-256 credential-generation fingerprint, never a full token. The winning operation re-reads canonical credentials, publishes a rotated pair before admitting another operation, shares success or failure with its current waiters, and removes its active entry immediately afterward. Waiter cancellation cannot abandon a provider-issued pair before persistence; graceful shutdown drains active work. Different connections do not hold a shared HTTP lock. Network/process failure is not claimed to be exactly-once. C17A-C17B migrated configured callers; C18 deleted the legacy token-keyed completed-result cache and dead provider-wrapper exports. |
| Foreground OAuth refresh callers | C17A routes chat 401 and structured token-authentication 403 recovery through the connection/generation coordinator, then reselects credentials from a fresh canonical snapshot under C13's existing generation-attempt budget. Permission/policy 403 responses do not rotate credentials. The Codex catalog helper shared with the foreground web-search check uses the same coordinator. Dead executor refresh hooks and the unused parallel `CredentialManager` state/lock maps were deleted. |
| Control/background OAuth refresh callers | C17B routes proactive refresh, quota/auto-ping, manual configured-account refresh, Kiro model discovery, Codex reset-credit operations, and provider connection tests through the same connection/generation coordinator. A proactive tick, quota request, and foreground 401 for one generation share one refresh; stale background results cannot overwrite a newer canonical generation. Provider connection tests retain their proxy/relay-aware refresh closure inside the coordinator. The manual route's no-connection bootstrap remains explicit and persists the new connection immediately; it cannot be connection-coordinated before identity exists. Idle coordinator entries remain zero. |
| Token-keyed refresh completed-result cache | C18 deleted `RefreshDedup`, `DedupEntry`, the full-old-token key, `OnceCell` wrapper, 10-second completed-result TTL, and unused `oauth::refresh` provider wrappers. The provider wire dispatcher retains only its bounded transient HTTP retry policy inside the active connection operation. Twenty thousand distinct stale credential generations and mass waiter cancellation leave zero idle entries and make no historical-token provider calls. No LRU, timer, or persisted replacement was added. |
| `healthCheckEnabled` | C24 preserves the legacy value; periodic probing becomes explicit opt-in while liveness remains independent of upstream availability. |
| `claudeAutoPing`/`codexAutoPing`/`glmAutoPing` and ping markers | C25 preserves opt-in and markers; no worker exists with no enabled connection. OAuth proactive refresh remains a distinct auth responsibility. |
| Token-keyed refresh/quota caches | C16-C18 replaced refresh coordination with active connection/generation singleflight and deleted its completed-result map and old-token keys. C23 separately owns the remaining quota cache. No persisted replacement is introduced. |

## Version evidence and limitations

- OpenCode installed during C00: `1.18.31`.
- Custom harness version/contract: `unknown`; real end-to-end compatibility is unverified until that external artifact is available.
- `PLAN.md` and `AUDIT-EVIDENCE.md` referenced by `docs/AGENT-PROMPT.md` are absent from this checkout. No evidence from them is inferred; the available executable graph is `docs/CHECKPOINTS.json`.

Unknown external versions do not block independent mock-based code corrections, but they must never be reported as a compatibility pass.
