# Synthetic dashboard visual references

`ui_parity.browser.test.mjs` records the Astro UI in the isolated browser
fixture described in [`.ui-browser/README.md`](../../.ui-browser/README.md).
Each recording compares screenshots from **two fresh contexts** at ImageMagick
AE=0 before writing the PNG. `reference/source.json` pins the Astro route HTML
hash, Chrome version and raster flags. Compare mode requires a Dioxus candidate,
not the Astro reference. Screenshots are 1440×900 desktop or 390×844 mobile,
light and dark, with fixed fonts, time, locale and device scale.

## Providers list slice

`UI_PARITY_SECTION=providers` selects these 28 captures without running the
other sections. `UI_PARITY_SCREEN` selects one full scenario name from the
first column; desktop-only scenarios produce two captures, the others four.
The default, unfiltered compare includes all reference sections; select a
section or screen when testing a partially migrated dashboard.

## Translator reference (work in progress)

`UI_PARITY_SECTION=translator` includes empty, populated, wrapped, Find, resized
Find, Replace, Go to Line (`:3:2`) and Go to Offset (`::9`, zero-based) editor states.
The resized Find state uses the real left sash; desktop widens to 539px while
the narrow mobile panel retains its 170px responsive width.
Recording the Astro reference requires the locally installed Monaco 0.55.1
`min/vs` directory mounted read-only at `/monaco-vs` (or set
`UI_MONACO_VS_DIR`); the test fulfills its CDN URLs from those local files and
blocks all other external requests. The Dioxus candidate does not ship Monaco.
These references self-repeat, but the candidate has not yet matched them at
AE=0 and its editor is not a full Monaco replacement.
The Go to Line/Offset fixtures focus the query after changing offset mode and
move the pointer away before capture so Monaco's transient toggle hover closes
through its normal interaction; no pixels or DOM are masked or hidden.

| Scenario | State captured |
|---|---|
| `providers-ready` | List at the top, with connected and disconnected custom providers and a connected OAuth provider. |
| `providers-search-match` | Desktop header search for mixed-case `fIxTuRe` leaves the two custom fixture cards. |
| `providers-search-empty` | Desktop header search for `no-such-fixture-provider` displays the no-results state. |
| `providers-free-filter` | Click “Show only free tier”, then return to the top. In this Astro build the filter removes its own Free Tier section and checkbox; custom providers remain. |
| `providers-scroll-api-keys` | Scroll to the API Key Providers section, including connected OpenAI and the initially collapsed provider list. |
| `providers-show-all-scroll-bottom` | Expand the API-key list and scroll the main container to its bottom, asserting it actually overflowed and its last card is visible. |
| `providers-add-openai-dialog` | Open the default OpenAI-compatible dialog; name and prefix are empty, Chat is selected, and Check/Create are disabled. |
| `providers-add-openai-dialog-scroll-bottom` | The same dialog scrolled internally to its lower fields and disabled actions. |

The header search is hidden at the mobile viewport in Astro, so the two search
states are desktop-only. The disappearing free-tier checkbox is a reference
behavior and an actionable UI gap: it cannot be unchecked in that state. This
test preserves the observation; it does not fix or invent a reset control.

Every Providers scenario uses the same synthetic responses:

- `GET /api/providers`: active OpenAI-compatible and OpenAI API-key connections,
  plus a successful Codex OAuth connection, with fixture IDs only and **no
  credentials**.
- `GET /api/provider-nodes`: a connected OpenAI-compatible Chat node named
  `Fixture Connected` and an unconnected Anthropic-compatible node named
  `Fixture Disconnected`.
- `GET /api/proxy-pools?isActive=true`: an empty pool list.
- `GET /api/auth/status`, `/api/settings`, `/api/build` and `/api/catalog`:
  authenticated synthetic shell, a fixed commit and the existing empty catalog.

Only those API/method/query combinations are fulfilled for Providers. The
fixture allows required same-origin page, bundle, font and `/providers/*.png`
assets; all other requests, including external origins and API writes, are
blocked and fail the scenario. The base URL displayed by the dialog is Astro's
default text: nothing is validated or sent to it.

This list slice does not cover provider detail, model customization and
persistence, connection creation/toggling and real OAuth, batch testing,
failure/loading/error/disabled states, an empty custom-provider list, the
Anthropic/API-key dialogs, dialog validation or submission, search clearing,
localization or real-backend behavior. No candidate parity is claimed for
Providers by recording these references.

## Add New Provider slice

`UI_PARITY_SECTION=providers-new` selects the 30 captures for
`/dashboard/providers/new`. Each scenario has desktop/mobile and light/dark
captures except the last, which is a mobile-only overflow state.

| Scenario | State captured |
|---|---|
| `providers-new-ready` | Nothing selected; “Connect Provider” is disabled. |
| `providers-new-search-match` | Mixed-case `cOdEx` filters the native select to the one Codex option. |
| `providers-new-search-retains-selection` | Select Codex, then search `no-such-provider`; the count is zero but the selected option and OAuth panel remain. |
| `providers-new-oauth` | Select `oauth:codex`; show the OAuth hint and “Start OAuth” without starting it. |
| `providers-new-no-auth` | Select `free:opencode-zen`; show “No auth required” and “Open Provider”. |
| `providers-new-api-key` | Select `apikey:openai`; show the blank name/key form and “Add Key on List”. |
| `providers-new-compatible` | Select `compatible:openai-compatible`; show the compatible setup hint and action. |
| `providers-new-api-key-scroll-bottom` | Mobile only: scroll the overflowing blank API-key form to its actions. |

These states use the provider options bundled in the Astro build, not live
account data. The only intercepted APIs are exact, queryless GETs for
`/api/auth/status`, `/api/settings`, `/api/build` and `/api/catalog`, returning
the authenticated synthetic shell, fixed commit and empty catalog. Required
same-origin page, bundle, font assets and the selected `codex.png`,
`opencode-zen.png` and `openai.png` provider icons are allowed; all other
requests, including API writes and external requests, are blocked and fail the
test. No provider credentials are entered; the key field's `sk-...` text is
only Astro's empty-field placeholder.

At the mobile viewport the Astro primary action is visibly compressed into a
thin strip (for example in `providers-new-oauth-mobile-dark.png`); the reference
records that existing appearance rather than silently fixing it.

The native select's open popup, provider navigation, OAuth, key entry and
submission, compatible setup navigation, the Anthropic-compatible option,
other provider categories, localization and backend success/error behavior
are not covered. These are Astro references, not Dioxus parity evidence.

## Kilo Code provider detail slice

`UI_PARITY_SECTION=provider-kilocode` selects 48 captures for
`/dashboard/providers/kilocode`: each scenario runs at both viewports and in
both themes. The model scenarios scroll the page's main container to the
Available Models card; the initial state stays at the top.

| Scenario | State captured |
|---|---|
| `provider-kilocode-ready` | The detail header, bundled free-tier information and an active synthetic connection. |
| `provider-kilocode-available-models` | The scrolled card with catalog, free, custom and disabled models, including a favorite and capability/context metadata. |
| `provider-kilocode-disable-model` | Disable the enabled catalog model `fixture-standard`; it appears with the pre-disabled models. |
| `provider-kilocode-restore-model` | Restore the disabled custom model `fixture-custom-disabled`; its saved custom name returns. |
| `provider-kilocode-free-only` | Enable Free only; the paid catalog rows disappear while the free and custom rows remain. |
| `provider-kilocode-add-model-dialog` | Open Add Custom Model and enter `kc/fixture-new-model`; the form shows the stripped ID and display prefix. No test is sent. |
| `provider-kilocode-add-model-saved` | Submit that model; its new custom row is visible. |
| `provider-kilocode-disable-all-dialog` | Open the confirmation for four active models, without confirming. |
| `provider-kilocode-device-oauth` | Open the device-code waiting dialog with synthetic URLs and code; `window.open` is stubbed and no provider is contacted. |
| `provider-kilocode-device-oauth-loading` | Hold the initial synthetic device-code response; check keyboard close and the candidate's nonvisual status. |
| `provider-kilocode-device-oauth-error` | Show a synthetic device-code failure; after capture, retry and cancel with the keyboard without contacting a provider. |
| `provider-kilocode-device-oauth-success` | Complete synthetic device polling while connection and node reads are held; show success and dismiss it with Done. |

Each fresh browser context receives synthetic, credential-free data:

- `GET /api/providers` returns one active Kilo Code OAuth connection with
  fixture metadata only; `/api/provider-nodes` and
  `/api/proxy-pools?isActive=true` are empty.
- `GET /api/catalog` maps `kilocode` to `kc` and returns `fixture-standard`,
  `fixture-vision` and `fixture-disabled`; `/api/providers/kilo/free-models`
  returns `fixture-free` without calling Kilo Code. `/api/models` supplies
  fixture capability metadata.
- `GET /api/models/custom` returns named `fixture-custom` and
  `fixture-custom-disabled`; `GET /api/models/disabled?providerAlias=kc`
  initially disables the latter and `fixture-disabled`. The initial empty
  `providerAlias=` read is also allowed while Astro mounts.
  `/api/models/alias` is empty; `/api/models/favorites` marks
  `fixture-standard` as a favorite. `/api/providers/filters` starts with
  `kc.freeOnly=false`.
- Queryless `GET /api/auth/status` and `/api/settings` provide the authenticated
  shell. `GET /api/build` returns `{ "commit": null }`, the backend's supported
  missing-build-metadata case; the desktop sidebar therefore shows `—`. This
  avoids observed nondeterministic pixel differences when the commit label
  changes after navigation, without masking any pixels. Only the detail route, its
  `/providers/kilocode.png` icon, and normal same-origin bundles, fonts and
  shell assets are allowed through.

Only the exact mutation for the selected scenario is accepted, with its full
method, query and JSON body checked: `POST /api/models/disabled` for
`kc/fixture-standard`, `DELETE /api/models/disabled?providerAlias=kc&id=fixture-custom-disabled`,
`PUT /api/providers/filters` with `freeOnly=true`, or `POST /api/models/custom`
with the stripped ID. Responses and subsequent reads use state held **only in
that browser context's fixture**; it is reset before the second capture. After
each mutation screenshot the test reloads and verifies the UI against those
in-memory responses, not SQLite persistence.
The device-dialog cases allow only synthetic device-code and, for the success
case, poll responses; the Open button is checked against a stub, the loading
case holds the response and checks close, and the error case checks retry and
cancel.
All other requests and mutations—including
`/api/models/test`, real OAuth/provider traffic and external origins—are
blocked and fail the test.

At 390px the enabled model IDs are truncated to similar `kc/fixtur...`
labels, and the custom-model dialog's Test button reaches past its content
edge. These are existing Astro appearances preserved by the reference.

This slice does not cover real backend persistence or `/v1/models`, adding or
testing connections, real OAuth, custom-model test/delete, confirming Disable All,
Active All, alias/favorite changes, errors, other providers, or localization.
The screenshots are Astro references, not a claim of Dioxus detail parity.

## OpenAI-compatible provider detail slice

`UI_PARITY_SECTION=provider-compatible` selects 16 captures of the dynamic
detail route: four states at desktop/mobile sizes in light/dark themes.

| Scenario | State captured |
|---|---|
| `provider-compatible-ready` | A compatible node with no connections and one synthetic custom model. |
| `provider-compatible-edit-dialog` | The edit form with existing name, prefix, Chat Completions and base URL; check-key and model-ID fields are empty. |
| `provider-compatible-edit-dialog-scroll-bottom` | The same form scrolled internally to its Save and Cancel controls, which are below the initial viewport at both sizes. |
| `provider-compatible-delete-dialog` | The confirmation naming the node; deletion is not confirmed. |

Each fresh context receives a synthetic pre-existing node with ID
`openai-compatible-00000000-0000-4000-8000-000000000001`, name
`Fixture Compatible`, prefix `fixture-openai`, type `openai-compatible`,
`apiType: "chat"`, and base URL `https://fixture.invalid/v1`. That reserved
invalid URL is display data only; no credential is entered and no provider
traffic is sent. The fixture includes no connections and one custom model,
`fixture-openai/fixture-model`.

The request navigates to `/dashboard/providers/<fixture-id>`; the backend
serves Astro's real `dashboard/providers/_dynamic.html` template. Record mode
requires its pre-pinned hash in `reference/source.json` **and** the
`ProviderDetailPageClient` island marker. The fixed synthetic reads are exact,
queryless GETs for `/api/auth/status`, `/api/settings`, `/api/build`,
`/api/catalog`, `/api/providers`, `/api/provider-nodes`, `/api/providers/filters`,
`/api/models`, `/api/models/custom`, `/api/models/alias` and
`/api/models/favorites`; plus `/api/proxy-pools?isActive=true` and
`/api/models/disabled?providerAlias=` or the exact fixture ID. The build
response has `commit: null` as in the Kilo detail slice. The exact same-origin
Astro bundles, fonts, favicon and `/providers/oai-cc.png` observed in this
frozen fixture are enumerated in the test; all other requests, WebSockets and
mutations are blocked. The test never clicks Check, Save, Delete, Add API Key
or Import from /models.

For a later Dioxus comparison, provide that build's exact same-origin hashed
static paths as the JSON array `UI_PARITY_DETAIL_ASSETS` (passed into the
browser container with `-e`), for example
`["/assets/openproxy-dashboard-dxh123.js","/assets/openproxy-dashboard_bg-dxh123.wasm"]`.
Only paths under `/assets/`, `/_astro/`, or `/i18n/` with no query, fragment,
or traversal are accepted. Candidate paths are never allowed during recording.
The previous name `UI_PARITY_COMPATIBLE_ASSETS` remains supported as a fallback.

This deliberately covers a prefixed node already present in the read fixture,
not creation or real persistence. An existing create-to-detail gap needs
separate investigation: the current backend `POST /api/provider-nodes`
generates an unprefixed UUID, while Astro's compatible-detail predicate checks
for an `openai-compatible-` ID prefix. Saving/deleting, validation and errors,
API-key connections, model changes, Responses/Anthropic-compatible nodes and
real backend behavior are not covered. These are Astro references, not Dioxus
parity evidence.

## Codex connection auto-ping slice

`UI_PARITY_SECTION=provider-codex-autoping` selects eight captures of
`/dashboard/providers/codex`: two states at desktop/mobile sizes in light and
dark. The OAuth connection and its auto-ping button must be in the screenshot;
the test scrolls the button into view if necessary.

| Scenario | State captured |
|---|---|
| `provider-codex-autoping-off` | Auto-ping is **off** for `Fixture Codex`, although the aggregate `enabled` is true because a different synthetic connection ID is on. |
| `provider-codex-autoping-on` | Start with both synthetic IDs off, click Auto-ping, and reload. The selected connection is **on** and the other ID remains off. |

Every fresh context has one active, successful, credential-free Codex OAuth
connection (`fixture-codex-connection`, named `Fixture Codex`) and a synthetic
`cx/fixture-model` catalog entry. There is no real OAuth session. The settings
fixture uses `{ enabled, connections }` under `codexAutoPing`, with the second
ID `fixture-other-codex-connection` present only in that map; `glmAutoPing` is
disabled. This deliberately checks the per-connection value rather than
assuming the aggregate `enabled` applies to every connection.

Both cases assert the correct accessible label, `aria-pressed`, enabled button
and the Codex tooltip text. The on case accepts only two queryless
`PATCH /api/settings` requests, in order. The first has exactly this JSON body:

```json
{
  "codexAutoPing": {
    "enabled": true,
    "connections": {
      "fixture-codex-connection": true,
      "fixture-other-codex-connection": false
    }
  }
}
```

The second changes both `enabled` and `fixture-codex-connection` to `false`,
leaving the other ID `false`. Each complete JSON body is checked before the
fixture updates its context-local settings. The screenshot is taken only after
a reload confirms the on state from that fixture; after capture, the test clicks
off and reloads again to confirm the false state. The off scenario allows no
PATCH. This tests client read-back from an in-memory mock, **not** SQLite
persistence or restart durability.

All API responses are synthetic and intercepted, never forwarded. The exact
queryless GETs are `/api/auth/status`, `/api/settings`, `/api/build`,
`/api/catalog`, `/api/providers`, `/api/provider-nodes`,
`/api/providers/filters`, `/api/models`, `/api/models/custom`,
`/api/models/alias`, `/api/models/favorites`, and
`/api/oauth/codex/stop-proxy`; plus `/api/proxy-pools?isActive=true` and
`/api/models/disabled?providerAlias=` or `?providerAlias=cx`. Astro's closed
OAuth modal calls the last OAuth URL during mount, despite not starting OAuth.
Although its method is GET, the real endpoint stops a listener: the fixture
fulfills it with `{ "success": true }` and **never forwards it**. The build
commit is null, as for the other detail fixtures. The test pins the Codex Astro
HTML hash and island marker, permits only its exact detail page, local Codex
icon and enumerated same-origin Astro bundles and fonts, and blocks all other
requests, external origins, WebSockets and other mutations. For a Dioxus comparison,
`UI_PARITY_DETAIL_ASSETS` supplies only the candidate's exact static paths as
described above; it does not relax the API allowlist.

No warmup, quota tick, real provider traffic, credentials or production data are
used. Saving failures, migration of legacy settings, the saving/loading state,
multiple visible accounts, GLM/API-key eligibility, tooltip hover, OAuth,
scheduler behavior and real-backend persistence are outside this slice. These
are Astro references, not evidence of Dioxus parity.

## Codex Bulk Add dialog slice

`UI_PARITY_SECTION=provider-codex-bulk-add` or
`UI_PARITY_SCREEN=provider-codex-bulk-add-dialog` selects four captures of the
open dialog on `/dashboard/providers/codex`, at desktop/mobile sizes in light
and dark. Each recording compares two fresh contexts at AE=0, without masking.

| Scenario | State captured |
|---|---|
| `provider-codex-bulk-add-dialog` | Click Bulk Add for the existing `Fixture Codex` connection. The text area is empty, `Import All` is disabled, and `Close` is enabled; neither is clicked. |

This reuses the Codex auto-ping fixture and its exact API/asset allowlists above,
with both synthetic connection IDs off. The only apparent JSON is Astro's
built-in **placeholder**: its ellipses and example token strings are not real
credentials, and nothing is pasted or typed. The test does not submit an import,
open OAuth, or close the dialog. In the mobile reference the placeholder wraps
within the textarea as in Astro.

Every API response, including the side-effecting
`GET /api/oauth/codex/stop-proxy`, is fulfilled in the browser rather than sent
to the backend. Recording asserts that Astro's mount-time stop request was
intercepted in each fresh context. This scenario accepts **no API mutations**;
an import, another OAuth request, an external request, a WebSocket or any
unlisted request is blocked and fails the test. Service workers are blocked.
Only the enumerated queryless same-origin page and static assets reach the
fixture server, and their redirects are rejected before Chrome can follow them.
For these Codex assets Playwright verifies the fixture's DNS-constrained CA;
Chrome uses the fixture's separate disposable trust profile. The internal
Compose network has no external gateway, and no production data or credentials
are mounted. The fixture is not a security boundary against a hostile host or
Docker administrator.

Record mode requires the existing pinned Astro Codex HTML hash and island marker;
it does not change an already pinned source hash. This slice covers only the
initial empty dialog, not parsing, validation, import, results, OAuth, persistence,
or real-backend behavior. These are Astro references, not a claim of Dioxus
parity.

## Codex manual OAuth dialog slice

`UI_PARITY_SECTION=provider-codex-oauth` selects eight desktop/mobile,
light/dark captures of the manual authorization dialog before and after its
synthetic authorize response. The ready state compares both an open and a
blocked stub popup against the same four references, without contacting a
provider. After the blocked-popup capture, copying the URL, entering a manual
callback and cancelling are checked. In the loading state, the response is
held and the test checks that Dioxus cannot exchange an entered callback before
it arrives. Other OAuth states are outside these scenarios.

## OAuth callback slice

`UI_PARITY_SECTION=callback` records ten captures of the standalone Astro
`/callback` page: five scenarios at both desktop and mobile sizes. Astro uses
the same hard-coded dark styling for both OS color schemes, so the stored PNGs
use the `-light` suffix only. For each scenario the harness also captures a
fresh dark-scheme context and compares it to the same complete reference at
AE=0; it does not create a duplicate dark PNG or mask pixels.

| Scenario | Query and state captured |
|---|---|
| `callback-code` | `code=fixture-code&state=fixture-state`: authentication complete and the close-window message. |
| `callback-error` | `error=fixture-error`, `error_description=Fixture authorization denied` and `state=fixture-state`: authentication failed with the description. |
| `callback-no-auth-data` | No query: the missing-data message and Callback URL instructions. |
| `callback-manual-copy` | The synthetic code/state query with no relay or opener: the full fixture URL and Copy URL button. |
| `callback-manual-copied` | The same fallback after clicking Copy URL: the temporary “Copied!” button state. |

The normal cases run with native BroadcastChannel and localStorage in an
isolated context but no opener; they have no real OAuth session or recipient.
For the manual cases an initialization script makes BroadcastChannel and
localStorage unavailable, with `window.opener` still null. The copied case
replaces the clipboard with an in-memory stub and asserts it receives exactly
`https://app:4623/callback?code=fixture-code&state=fixture-state`; it never
touches the system clipboard. These values are synthetic, including those
visible in the fallback PNG.

The Astro page has no island or dashboard font assets. Record mode requires its
standalone markup **and** the pre-pinned SHA-256 of the served HTML in
`reference/source.json`; other Astro island checks remain unchanged. Compare
mode still requires the Dioxus loader. The pinned browser image's system
Liberation Sans is verified for rendered callback text via Chrome DevTools;
no dashboard-font substitution is introduced. Record mode allows only the
exact same-origin `/callback` GET for the selected synthetic query. Compare
mode additionally allows queryless same-origin dashboard bundle/font assets;
all API requests, writes, external requests and WebSockets are blocked.

No real provider request or code exchange is made. These visuals do not test
message delivery to an opener or another tab, the auto-close timer, storage
cleanup, clipboard failure/legacy copy, the reset of “Copied!”, mixed
code/error parameters, or relay failures combined with the other results.
They are Astro references, not evidence of Dioxus callback parity.
