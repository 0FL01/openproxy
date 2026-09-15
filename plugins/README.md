# OpenCode model discovery

`openproxy-models.js` fetches OpenProxy's `/v1/models` when OpenCode loads its
configuration. Tested with OpenCode **1.18.31**. No npm dependencies, credentials
in the plugin, generated JSONC, polling, or persistent client cache.

## Install

From the repository root, copy **only the plugin**, not the tests:

```sh
mkdir -p ~/.config/opencode/plugins
cp plugins/openproxy-models.js ~/.config/opencode/plugins/openproxy-models.js
```

OpenCode auto-loads this directory; no `plugin` config entry is needed. The
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
        "chunkTimeout": 6000000,
        "headers": { "X-OpenProxy-Codex-Web-Search": "true" }
      }
    }
  }
}
```

Set `LUDKA2_API_URL` (including `/v1`) and `LUDKA2_API_KEY` in the environment
that launches OpenCode. The plugin uses the resolved provider options and sends
the same API key as `Authorization: Bearer …`. Discovery has its own 10-second
timeout, independent of inference timeouts. HTTP redirects are not followed.

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

The router determines which IDs exist. On success the plugin replaces the
in-memory list, excluding removed/disabled models even if locally configured.
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

The updated router supplies an additive `opencode` object on `/v1/models` rows:
name, canonical `source`, limits, modalities, reasoning/tool support, and
reasoning-effort variants.
It reuses static, models.dev and Codex metadata. Missing metadata is **not
guessed** from model names or from one member of a combo. Input limits and some
output limits may be unknown. The plugin warns about missing context/output
limits; retain local overrides until the proxy has the correct values. An older
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

## Verification

```sh
node --test tests/opencode_models.test.mjs
OPENCODE_BINARY="$(command -v opencode)" node --test tests/opencode_models_cli.test.mjs
cargo test --lib v1_models
cargo test --test models_custom_api --test api_auth_and_models
```

The CLI smoke test uses temporary HOME/XDG directories and a loopback fixture,
not your proxy, credentials, or OpenCode config.
