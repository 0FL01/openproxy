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

Not yet covered: provider detail or `/providers/new` pages, model customization
and persistence, connection creation/toggling and real OAuth, batch testing,
failure/loading/error/disabled states, an empty custom-provider list, the
Anthropic/API-key dialogs, dialog validation or submission, search clearing,
localization and real-backend behavior. No candidate parity is claimed for
Providers by recording these references.
