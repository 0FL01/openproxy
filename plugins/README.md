# OpenCode model discovery and usage sidebar

`openproxy-models.js` fetches OpenProxy's `/v1/models` when OpenCode loads its
configuration. `openproxy-tui.js` enables its compact usage-limits sidebar.
Tested with OpenCode **1.18.34**. Plain JavaScript: no package installation or
build step, embedded credentials, generated JSONC, or persistent client cache.
Solid/OpenTUI imports are lazy and supplied by the TUI host; model discovery
has no new runtime dependencies.

## Install

From the repository root, copy **both plugin files**, not the tests:

```sh
mkdir -p ~/.config/opencode/plugins
cp plugins/openproxy-models.js ~/.config/opencode/plugins/openproxy-models.js
cp plugins/openproxy-tui.js ~/.config/opencode/plugins/openproxy-tui.mjs
```

The wrapper is installed with an `.mjs` extension so OpenCode's server plugin
auto-discovery (`*.js`/`*.ts`) does not try to load its TUI-only export. Keep the
two installed files together. In **`~/.config/opencode/tui.json`** (or your
existing `tui.jsonc`), add the TUI file entry:

```jsonc
{
  "$schema": "https://opencode.ai/tui.json",
  "plugin": ["./plugins/openproxy-tui.mjs"]
}
```

Preserve other TUI settings and plugins. **Replace** the existing
`oc-usage-limits-plugin@1.6.1` entry (or another version of that plugin) with
this file entry to avoid duplicate panels and direct provider polling.

OpenCode auto-loads `openproxy-models.js`; no server `plugin` entry is needed. The
provider ID is `ludka2`; change `PROVIDER_ID` at the top of the plugin if needed.
Keep your existing provider options, including the SDK choice:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "ludka2": {
      "npm": "@ai-sdk/openai",
      "name": "ludka2",
      "options": {
        "baseURL": "{env:LUDKA2_API_URL}",
        "apiKey": "{env:LUDKA2_API_KEY}",
        "timeout": false,
        "setCacheKey": true,
        "chunkTimeout": 6000000
      }
    }
  },
  "mcp": {
    "codex_web": {
      "type": "remote",
      "url": "{env:LUDKA2_API_URL}/mcp",
      "enabled": true,
      "oauth": false,
      "headers": {
        "Authorization": "Bearer {env:LUDKA2_API_KEY}"
      },
      "timeout": 30000
    }
  }
}
```

Set `LUDKA2_API_URL` (including `/v1`) and `LUDKA2_API_KEY` in the environment
that launches OpenCode. The plugin uses the resolved provider options and sends
the same API key as `Authorization: Bearer …`. Discovery allows 15 seconds per
attempt within a 30-second total budget, independent of inference timeouts.
Cold-start network failures, HTTP 408/425/429/5xx, unreadable JSON, and empty
catalogs are retried up to three times after 250/750/1500 ms delays (bounded by
the remaining budget). HTTP redirects are not followed.
The remote MCP uses the same key directly—no local wrapper or search plugin is
required—and OpenCode exposes its only tool as `codex_web_search`. The proxy
uses the configured private Codex account against the standalone search index;
it does not run a Responses generation model for the search.

Quit and restart OpenCode after installation. To inspect the current catalog:

```sh
opencode models ludka2 --refresh
opencode models ludka2 --verbose
```

`--refresh` itself refreshes models.dev; our fetch happens on initialization and
also runs without that flag. A separate CLI process **does not update an already
open TUI**: restart the TUI to load additions/removals. Upstream provider discovery
inside OpenProxy retains its existing cache policy.

## Metadata and local overrides

The router determines which IDs exist. On a valid nonempty response the plugin
replaces the in-memory list, excluding removed/disabled models even if locally configured.
Local `models` entries override metadata for matching IDs (limits are merged
field by field); other provider options and other providers are untouched.
After merging, context and input limits above 500,000 tokens are capped at
500,000; lower upstream limits are preserved.
When neither the proxy nor a local override supplies a useful name, the plugin
removes the router prefix for display and formats the slug (for example,
`cx/gpt-5.6-luna` becomes `GPT-5.6 Luna`). The canonical source provider is
always appended, so equivalent routes remain distinct: `GPT-5.6 Luna · codex`
and `GPT-5.6 Luna · opencode-go`. The source comes from backend routing metadata,
never from the configurable model prefix. The model ID used for requests is
unchanged. Explicit proxy and local names retain priority before the source suffix.
Known acronym casing is preserved for the generated `GPT` and `GLM` names;
`Glm 5.2` from proxy metadata is normalized to `GLM 5.2`.
Repeated configuration hooks keep exactly one canonical source suffix; existing
trailing duplicates such as ` · GLM · glm` are collapsed to ` · glm` without
changing the base model name beyond acronym normalization.

The updated router supplies an additive `opencode` object on `/v1/models` rows:
name, canonical `source`, limits, modalities, reasoning/tool support, and
reasoning-effort variants.
It reuses static, models.dev and Codex metadata. Missing metadata is **not
guessed** from model names. Input limits and some
output limits may be unknown. The plugin silently omits incomplete context/output
limits to satisfy OpenCode's config schema; local overrides can fill the gaps.
Discovery failures still warn and retain configured models. An older
router still supports ID discovery and optional `context_length` /
`max_completion_tokens`, but may not supply the richer metadata or consistently
filter disabled custom models. Deploy the updated backend for those guarantees.

For custom models, send metadata using the existing authenticated management API
(`POST /api/models/custom`, or `PUT /api/models/custom/{id}` for an existing row):

```json
{
  "providerAlias": "my-proxy",
  "id": "my-model",
  "name": "My model",
  "opencode": {
    "limit": { "context": 628000, "input": 500000, "output": 128000 },
    "modalities": { "input": ["text", "image"], "output": ["text"] },
    "reasoning": true,
    "tool_call": true,
    "variants": {
      "medium": { "reasoningEffort": "medium" },
      "high": { "reasoningEffort": "high" }
    }
  }
}
```

These are example values, not defaults. Custom metadata is stored in the
existing SQLite custom-model payload and survives rebuilds. `PUT` replaces the
supplied `opencode` object; omit it to leave it unchanged, or send `{}` to clear
its overrides. No new dashboard fields are added. Use the existing management
authorization for edits; discovery needs only the inference API key. Do not
include credentials, SDK overrides or transport settings in model metadata.

On timeout, HTTP error or invalid data the plugin keeps the original configured
models and reports a sanitized warning. With no local entries there is no offline
fallback catalog. JSONC on disk is never changed.
An empty catalog is treated as a cold-start failure: if it stays empty after
retries, configured models are retained with a warning, even if the proxy has
intentionally disabled all models.

## Usage limits sidebar

The sidebar reads the same resolved `provider.ludka2.options.baseURL`, `apiKey`
and custom `headers` as discovery, respecting `disabled_providers` and
`enabled_providers`. It sends one authenticated `GET /v1/usage/limits` at a time
to OpenProxy with a 10-second timeout, a streaming 2 MiB response bound and
redirects disabled. It polls the proxy
every 60 seconds; an initial `loading` result gets one retry after 2.5 seconds.
OpenProxy owns provider credentials and upstream refreshes; its **180-second
cache TTL** permits a background refresh rather than indicating a failure. The
client does not contact providers directly.

The panel shows one block per provider, with a plan only when shared by all its
connections. Matching quota windows and units are combined: normalized percentage
quotas show mean usage, real counters sum used/total before calculating the
percentage, and balances sum remaining amounts. Different units remain separate.
The reset countdown shows the nearest known reset, when part of the pool renews.
Unknown values are not counted as zero; incomplete aggregates are marked.
Provider headings and percentages are bold. Each quota is one compact text row
with an eight-character thin green bar (`━` filled, `─` empty) and an inline reset
such as `↻2d3h`. Percentages are green below 70%, yellow from 70% and red from 90%;
reset details use muted theme colors.
Connection labels and routine freshness/age text are hidden; warnings identify
failures, partial, loading, unsupported or unavailable data. A cached `stale`
result with `error: null` is normal, including while a refresh is pending, and
does not warn. Any nonnull per-connection `error` appears as a short, sanitized
message under its provider, including when other connections succeed. Optional
`errorStatus` adds an HTTP code; `nextRefreshAt` adds a retry countdown, or
`refreshing: true` shows `retrying` for a failed refresh in progress. These
diagnostics do not trigger additional polls. Older backends remain supported;
without an error they cannot distinguish a failed refresh from TTL expiry.

The response is bounded to 128 connections; the panel reports when some limits
are omitted. Countdown text updates every 30 seconds. On proxy failure or
invalid data, the last valid response stays visible with a stale warning and a
sanitized failure message. Retained values also warn after 190 seconds without
a successful, fully validated proxy read (180 seconds plus the 10-second request
deadline grace, checked on the countdown tick). This uses the client's receipt
clock, never the server's `observedAt`, so server clock skew or an old upstream
observation cannot create false stale warnings. A successful read clears proxy
failure/silence warnings; an upstream error clears when the backend reports
`error: null`. Optional diagnostics validate atomically with the whole response.
Timers and active requests stop when the plugin is disposed. Without a backend
supporting this endpoint the panel shows unavailable. Quit and restart OpenCode
after installing or changing either file or the TUI config; open a session with
its sidebar visible to see the panel.

## Verification

```sh
node --test tests/opencode_models.test.mjs
node --test tests/opencode_models_sidebar.test.mjs
OPENCODE_BINARY="$(command -v opencode)" node --test tests/opencode_models_cli.test.mjs
OPENCODE_BINARY="$(command -v opencode)" node --test tests/opencode_models_tui_cli.test.mjs
cargo test --lib v1_models
cargo test --test models_custom_api --test api_auth_and_models
```

The CLI smoke tests use temporary HOME/XDG directories and loopback fixtures,
not your proxy, credentials, or OpenCode config. The TUI smoke test requires
Python 3 with Unix PTY support and verifies actual rendering and a reactive
loading-to-cached-refresh update in the installed binary without inference.
