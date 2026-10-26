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
The default, unfiltered compare includes all reference sections, including
Providers, which still needs a Dioxus implementation.

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

This list slice does not yet cover provider detail, model customization and
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
