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
| Process-wide Claude header cache | C04 deletes it without migration; only current-request allowlisted headers and explicit adapter defaults are used. |
| Kiro session-start replay | C05 deletes it without migration; the current request is the only content source. |
| Session/continuation maps | C06 retains only protocol-proven, byte-bounded continuation state; never touch dashboard auth `AppState.sessions`. |
| `settings.providerContextLimits` | C07-C08 preserve the map and metadata meaning while removing default autonomous chat rejection/headroom; explicit count-tokens remains a separate API. Empty currently means defaults, not off. |
| `modelLock_*`, cooldown/error, and health fields | C14/C24 preserve diagnostic values but do not let them select accounts or trigger success-path writes/default probes. |
| `healthCheckEnabled` | C24 preserves the legacy value; periodic probing becomes explicit opt-in while liveness remains independent of upstream availability. |
| `claudeAutoPing`/`codexAutoPing`/`glmAutoPing` and ping markers | C25 preserves opt-in and markers; no worker exists with no enabled connection. OAuth proactive refresh remains a distinct auth responsibility. |
| Token-keyed refresh/quota caches | C16-C18/C23 replace only active in-flight coordination, then delete completed-result maps and old-token keys; no persisted replacement is introduced. |

## Version evidence and limitations

- OpenCode installed during C00: `1.18.31`.
- Custom harness version/contract: `unknown`; real end-to-end compatibility is unverified until that external artifact is available.
- `PLAN.md` and `AUDIT-EVIDENCE.md` referenced by `docs/AGENT-PROMPT.md` are absent from this checkout. No evidence from them is inferred; the available executable graph is `docs/CHECKPOINTS.json`.

Unknown external versions do not block independent mock-based code corrections, but they must never be reported as a compatibility pass.
