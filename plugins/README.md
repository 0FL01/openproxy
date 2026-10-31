# OpenCode V2 model discovery

`openproxy-models.js` is the checked-in OpenCode V2 aggregate discovery/client
implementation. It uses host-provided `@opencode/plugin`,
`Plugin.define` and `setup(ctx)` to fetch OpenProxy's `/v1/models` and register
one `ctx.model.transform`, plus native-model-only AI SDK/session hooks. No package installation or build step, embedded
credentials, generated JSONC, or persistent client cache is required.
OpenCode V1 is not supported by this discovery plugin.

The old TUI/sidebar and subscription-impersonation integrations have been
retired, not migrated. This plugin does not rewrite client identity, system
identity, user agents, tool descriptions, OAuth fields or native credentials.

## Claude Agent native subscription

On the **served Astro dashboard**, open Providers → Claude Agent → **Add Account**.
Start native login, follow the unmodified server-side `claude auth login` output,
and open/copy its native HTTPS link. Enter a password only at Anthropic. When the
native process requests interactive input (including a browser-returned code),
the form forwards it with a newline. Closing the modal cancels that specific
pending attempt; authentication is successful only after the server reports
native status/probe success. Connections and model inventory then refresh through
the existing provider/catalog paths. There is no token import or app-owned OAuth
form. Native credentials remain only in the server's persistent native profile.

Claude Agent appears in the existing aggregate **`ludka2`** provider. **No Claude
installation or Claude login is needed on the OpenCode client.** The original
Claude binary and official Agent SDK run on the proxy server; OpenCode remains
the history, tool-execution, permission and compaction host. Subscription identity
is not inferred from displayed prices.

Only rows whose canonical **`opencode.source` equals `claude-agent`** select the
locally defined remote LanguageModelV3 adapter. A route prefix or display name
never selects it. The transform sets the fixed `aisdk:@openproxy/claude-agent`
sentinel and a provider-scoped `ctx.aisdk.hook("sdk", …)` supplies its SDK in
memory: there is **no package to download**. Catalog-supplied SDK/package, URL,
headers and options are still ignored, including on native rows. Unrelated models
keep their original transports and hooks.

The adapter requires OpenCode **V2** with the public Promise-context
`ctx.aisdk.hook`, `ctx.session.hook`, `ctx.agent.get`, and terminal execution
events. Its session/instruction behavior follows OpenCode PR #58782 at
`f29e281322b35b525e337d12e84aedc7e129fd6d`, specifically
`packages/core/src/plugin/provider/claude-agent/{aisdk,session,instructions}.ts`.
It preserves primary/title/generate/compaction kind, client session/fork source,
resolved agent role, keyed project instructions, tool-error/aborted flags,
attachments and cancellation. The role comes from `ctx.agent.get`, and instruction
sources from `metadata.experimental.instructions`; system-text parsing is not
used. The remote server's cwd is explicitly **not** the client workspace.

Native requests use authenticated `POST /v2/claude-agent/stream` and bounded,
validated `application/x-ndjson` LanguageModelV3 parts. Binary attachments become
base64. Missing/malformed framing or terminal finish is an error, not a partial
success. Only the OpenProxy key is sent, never Claude credentials. Configured
reverse-proxy prefixes are retained: `https://host/proxy/v1` becomes
`https://host/proxy/v2/claude-agent/stream`, not `https://host/v2/…`.
OpenCode's extra generation retry and automatic chat maxTokens are suppressed
only for native models; native Claude owns its retry/output-budget behavior.
Explicit side-call budgets remain intact. `POST /v2/claude-agent/idle` is sent
only after `session.execution.succeeded`, `.failed` or `.interrupted`. A tool-call
stream ending does **not** arm idle while a client tool awaits approval.

This is not arbitrary-client OpenAI chat compatibility or a V1 shim. The new
runtime is supported only through the V2 adapter. The Anthropic API-key provider
remains separate and unchanged.

## Install discovery for OpenCode V2

From the repository root, copy **only the discovery plugin**, not the tests:

```sh
mkdir -p ~/.config/opencode/plugins
cp plugins/openproxy-models.js ~/.config/opencode/plugins/openproxy-models.js
```

OpenCode V2 automatically discovers direct `.js`/`.ts` files under
`~/.config/opencode/plugins/` (or `$XDG_CONFIG_HOME/opencode/plugins/`) and
project `.opencode/plugins/`. No explicit `plugins` entry is needed for that
location. A repository-root `plugins/` directory is not automatically discovered.
See the [V2 plugin loading guide](https://opencode.ai/v2/docs/build/plugins).

Keep the existing `ludka2` provider and its transport configuration. This plugin
does not create or reconfigure providers or their remote headers/options. The
fixed local native-model adapter above is its sole SDK-selection exception;
the model editor applies its changes only where the provider is
available. Remove obsolete sidebar file entries from your client plugin settings
when retiring a V1 installation; discovery no longer exports that sidebar.

Set `LUDKA2_API_URL` (including `/v1`) and `LUDKA2_API_KEY` directly in the
environment of the OpenCode service/plugin host before startup, for example:

```sh
export LUDKA2_API_URL='http://127.0.0.1:4623/v1'
export LUDKA2_API_KEY='your-openproxy-api-key'
opencode service restart
opencode
```

Use your own URL and key; do not put credentials in the plugin or commit them.
If a background service is already running, setting variables only in an attached
TUI's shell does not update that service's environment: restart it from the
intended environment. Discovery reads these variables during `setup`, **not**
resolved provider settings. It sends the key as `Authorization: Bearer …`. URLs with credentials,
query strings, fragments, or non-HTTP(S) schemes are rejected.

Discovery allows 15 seconds per attempt within a 30-second total budget,
independent of inference timeouts.
Cold-start network failures, HTTP 408/425/429/5xx, unreadable JSON, and empty
catalogs are retried up to three times after 250/750/1500 ms delays (bounded by
the remaining budget). HTTP redirects are not followed.

Discovery fetches once per plugin setup, not periodically. Restart the service
after installation or catalog changes to run setup again, then inspect the model
picker. There is no additional hot-reload guarantee. OpenProxy retains its
existing upstream discovery cache policy. The separate `/v1/mcp` search endpoint
does not require a discovery or local search wrapper plugin.

## Metadata and local overrides

On a valid nonempty response the transform removes existing `ludka2` IDs absent
from the catalog and updates or adds catalog rows. Other providers are untouched.
V2's later ConfigProvider explicit overrides take precedence; the plugin does
not redesign source registration or configuration ordering.

For non-native sources, proxy limits merge field by field with the model editor's existing limits. The
plugin writes a merged limit only when both context and output are positive
integers. At that point context and input above 500,000 tokens are capped at
500,000; lower values and the output limit are preserved. Later explicit config
overrides are not recapped by this transform.

Canonical `claude-agent` rows instead preserve the reported native context
uncapped (or keep Model.Info's default context when absent), set output to `0`
for unknown, and use subscription cost `[]`. Their default capabilities are
tools/text-image-PDF input and text output. The `claude-haiku` family is selected
only from the upstream model ID after the route prefix, never from that prefix.

When proxy metadata has no useful name, the plugin removes the router prefix
for display and formats the slug (for example,
`cx/gpt-5.6-luna` becomes `GPT-5.6 Luna`). When supplied, the canonical source
provider is appended, so equivalent routes remain distinct: `GPT-5.6 Luna · codex`
and `GPT-5.6 Luna · opencode-go`. The source comes from backend routing metadata,
never from the configurable model prefix. The model ID used for requests is
unchanged. Explicit proxy names take priority over generated names; later local
overrides are owned by OpenCode. Older rows without `opencode.source` have no
source suffix rather than guessing from the route prefix.
Known acronym casing is preserved for the generated `GPT` and `GLM` names;
`Glm 5.2` from proxy metadata is normalized to `GLM 5.2`.
Repeated transforms keep exactly one canonical source suffix; proxy-supplied
trailing duplicates such as ` · GLM · glm` are collapsed to ` · glm` without
changing the base model name beyond acronym normalization.

The updated router supplies an additive `opencode` object on `/v1/models` rows:
name, canonical `source`, limits, modalities, reasoning/tool support, and
reasoning-effort variants. V2 maps modalities to `capabilities.input/output`,
tool support to `capabilities.tools`, and enabled variants to
`[{ id, settings: { reasoningEffort } }]`; disabled variants are omitted. An
explicit empty variant object clears variants. Legacy attachment/reasoning
booleans are validated but not emitted as unsupported V2 fields.
It reuses static, models.dev and Codex metadata. Missing metadata is **not
guessed** from model names. Input limits and some
output limits may be unknown. Incomplete context/output pairs leave the model
editor's existing limits unchanged; existing output limits can fill the gaps.
Discovery failures warn and leave native models untouched. An older
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
include credentials, SDK overrides or transport settings in model metadata. The
plugin copies only the supported metadata whitelist, never remote SDK, URL,
header, or options overrides. Invalid supported metadata or duplicate IDs reject
the entire catalog before a transform is registered.

On timeout, HTTP error or invalid data the plugin keeps native models and reports
a sanitized warning without response bodies, raw network errors, URLs or keys.
It supplies no offline fallback catalog. JSONC on disk is never changed.
An empty catalog is treated as a cold-start failure: if it stays empty after
retries, native models are retained with a warning, even if the proxy has
intentionally disabled all models.

## Verification

```sh
node --test tests/opencode_models.test.mjs
node --test tests/opencode_models_native.test.mjs tests/claude_native_dashboard.test.mjs
pnpm --dir web run build
cargo test --lib v1_models
cargo test --test models_custom_api --test api_auth_and_models
```

The standalone discovery/client tests run on Node 22.23.3 and use a Node-native module
resolution shim for identity `Plugin.define`, plus a minimal model editor,
loopback HTTP and mocked responses. They import the unchanged runtime plugin
without installing the host package. They do not use the live proxy, private
credentials, or OpenCode config. Native tests add public-context hook fixtures,
actual loopback HTTP streaming, cancellation/framing checks, and dashboard
controller/SSR rendering fixtures. They are **not** proof of integration with a
real running OpenCode V2 service or an owner's authenticated subscription. Those
end-to-end checks are separate. The V1 CLI/TUI/sidebar tests were retired rather
than ported.
