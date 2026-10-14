# OpenCode V2 model discovery

`openproxy-models.js` is the working OpenCode V2 discovery implementation copied
from the installed client plugin. It uses host-provided `@opencode/plugin`,
`Plugin.define` and `setup(ctx)` to fetch OpenProxy's `/v1/models` and register
one `ctx.model.transform`. No package installation or build step, embedded
credentials, generated JSONC, or persistent client cache is required.
OpenCode V1 is not supported by this discovery plugin.

The old `openproxy-tui.js` wrapper and its usage sidebar have been retired,
not migrated. `openproxy-claude-mask.js` remains an independent **V1-only**
implementation; it has not been ported or verified for V2. Do not install it as
part of this V2 discovery setup. The discovery marker includes Claude models in
the catalog; it does not implement the mask's inference hooks.

## Legacy Claude Code client mask (ludka2, V1 only)

`openproxy-claude-mask.js` registers client-side masking hooks so requests to
the OpenProxy provider present themselves as a genuine Claude Code CLI run:

- `experimental.chat.system.transform` — replaces the OpenCode system prompt
  with the live Claude Code 2.1.289 prompt (identity + harness + style blocks;
  server-side ground truth) plus a Claude Code-shaped `# Environment` block,
  for `ludka2` models only. Host-specific paths are rebuilt at runtime from
  the real machine: the memory path is
  `<home>/.claude/projects/<working-directory-with-dashes>/memory/` and the
  environment block carries OpenCode's own session working directory, git
  status, platform, shell and OS version, so no corpus placeholder (e.g.
  `/home/user/...`) ever reaches the provider. The per-session working
  directory comes from OpenCode's session record (`client.session.get`) with
  the plugin instance directory as fallback. A fifth block mirrors the
  active OpenCode agent profile as a Claude Code permission mode: the `plan`
  agent gets the Claude Code plan-mode instructions (read-only planning,
  ExitPlanMode workflow), every other profile — including custom primaries
  and subagents — gets the `While bypass permissions mode is active` block
  a real CC session of that shape runs with. The agent name is remembered
  from the same request's `chat.headers`/`chat.params` hooks, which carry it
  before `system.transform` fires. Project instructions (AGENTS.md /
  CLAUDE.md / CONTEXT.md and global instruction files) survive the
  replacement: the `Instructions from:` sections OpenCode embeds in the
  system string are repacked under the verbatim Claude Code wrapper
  (`Codebase and user instructions are shown below…` / `Contents of <path>
  (project instructions, checked into the codebase):`). Global config
  paths are rewritten to their `~/.claude` form so no client-identity
  marker leaks.
- `chat.headers` — pins `User-Agent: claude-cli/2.1.289 (external, cli)` and
  sends `X-OpenProxy-Claude-Mask: 1` on `ludka2` requests.
- `tool.definition` — scrubs client-identity strings (`opencode`, `opencode.ai`,
  `anomalyco/opencode`) from tool descriptions sent to any provider. The bash
  tool's pre-created temp-directory grant (whose concrete
  `…/opencode-tmp/opencode` path the word scrub would mangle into a
  nonexistent `Claude Code-tmp/Claude Code`) is replaced wholesale with the
  verbatim Claude Code `$TMPDIR` guidance real CC sessions carry.

Other providers keep OpenCode's real identity. The proxy's server-side harness
spoof remains authoritative for unmasked traffic; a confirmed personalized mask
prompt (marker header + Claude Code identity, e.g. when an SDK hop dropped the
pinned CLI User-Agent) passes through byte-unchanged, preserving the client's
host-specific environment, mode and instructions. The client mask just removes
the fingerprints before they ever leave the machine.

**Server-side gate.** OpenProxy serves claude/anthropic models only to clients
that present as Claude Code: the mask marker header, or a genuine
`claude-cli/…` User-Agent. Unmasked clients get no `cc/*` rows in `/v1/models`
and a `403` on direct claude inference — so a session without the mask plugin
never burns subscription quota with flagged traffic. The V2 discovery plugin
sends the same marker on `/v1/models` so its catalog includes Claude models;
that alone does not mark inference requests. Legacy mask tests:
`node --test tests/opencode_mask.test.mjs`.

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
does not create or reconfigure providers, SDK packages, headers, or request
options; the model editor applies its changes only where the provider is
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
resolved provider settings. It sends the key as `Authorization: Bearer …` and
adds `X-OpenProxy-Claude-Mask: 1` to the catalog request. URLs with credentials,
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

Proxy limits merge field by field with the model editor's existing limits. The
plugin writes a merged limit only when both context and output are positive
integers. At that point context and input above 500,000 tokens are capped at
500,000; lower values and the output limit are preserved. Later explicit config
overrides are not recapped by this transform.

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
node --test tests/opencode_mask.test.mjs
cargo test --lib v1_models
cargo test --test models_custom_api --test api_auth_and_models
```

The standalone discovery tests run on Node 22.23.3 and use a Node-native module
resolution shim for identity `Plugin.define`, plus a minimal model editor,
loopback HTTP and mocked responses. They import the unchanged runtime plugin
without installing the host package. They do not use the live proxy, private
credentials, or OpenCode config. The independent mask suite remains legacy-only.
The V1 CLI/TUI/sidebar tests were retired rather than ported; no V2 service or
rendering smoke test is claimed by these checks.
