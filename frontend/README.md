# Dioxus dashboard migration slice

This is a separate, browser-only Rust/WASM application. It implements `/login`,
the standalone `/callback` OAuth relay,
the shared dashboard shell and Endpoint page at `/`, `/dashboard`, and
`/dashboard/endpoint`, the list and chooser at `/dashboard/providers` and
`/dashboard/providers/new`, Skills at `/dashboard/skills`, Console Log at
`/dashboard/console-log`, Settings at `/dashboard/profile`, DB Backups at
`/dashboard/db-backups`, Application Logs at `/dashboard/logs`, and a partial
`/dashboard/providers/:id` detail page
against the existing same-origin APIs. It does not replace the Astro frontend
or alter how OpenProxy is packaged or served. Other dashboard pages are not
implemented; their links remain ordinary browser navigations, not
substitute pages. Login and logout use full browser redirects, as the current
frontend does.

## Build and test

Use Rust 1.98.1 (the tested frontend toolchain) with the `wasm32-unknown-unknown`
target and **Dioxus CLI 0.7.10** (for example,
`cargo install dioxus-cli --version '=0.7.10' --locked`). Then, from this
directory:

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --target wasm32-unknown-unknown -- -D warnings
./scripts/build.sh
```

The independently locked build is emitted under
`target/dx/openproxy-dashboard/release/web/public/`. The Dioxus-generated WASM
and JavaScript bridge filenames may include content hashes and may change on
rebuild; they are build artifacts and are ignored, not source files. The CLI
may download matching build tools to its own user cache. Incremental `dx`
builds can retain older, unreferenced hashed assets (and old files formerly
in `public/`) in this output directory. Use `./scripts/build.sh`, rather than a
bare `dx build`, for the served or staged output: after building, it restores
the original provider PNG bytes that Dioxus otherwise re-encodes, verifies them,
and fails if a stale provider PNG exists in the output. Set `DX` to select a
specific CLI executable. A future packaging step must stage a clean build output
rather than treating this cache as a production-ready tree.

An optional isolated behavioral smoke test is available after the release build:

```sh
# Install Playwright separately and provide a sandbox-capable Chromium if needed.
CHROME_BIN=/path/to/chromium node tests/login-smoke.cjs
CHROME_BIN=/path/to/chromium node tests/endpoint-smoke.cjs
CHROME_BIN=/path/to/chromium node tests/providers-smoke.cjs
CHROME_BIN=/path/to/chromium node tests/providers-new-smoke.cjs
CHROME_BIN=/path/to/chromium node tests/provider-detail-smoke.cjs
PROVIDER_AUTOPING_ONLY=1 CHROME_BIN=/path/to/chromium node tests/provider-detail-smoke.cjs
PROVIDER_CODEX_CONTROLS_ONLY=1 CHROME_BIN=/path/to/chromium node tests/provider-detail-smoke.cjs
PROVIDER_NODE_ONLY=1 CHROME_BIN=/path/to/chromium node tests/provider-detail-smoke.cjs
PROVIDER_IMPORT_ONLY=1 CHROME_BIN=/path/to/chromium node tests/provider-detail-smoke.cjs
PROVIDER_CALLBACK_ONLY=1 CHROME_BIN=/path/to/chromium node tests/provider-detail-smoke.cjs
PROVIDER_POPUP_ONLY=1 CHROME_BIN=/path/to/chromium node tests/provider-detail-smoke.cjs

# Optional snapshot drift check, with the repository's web dependencies installed:
node tests/provider-metadata.cjs
```

The smoke tests require Node and the `playwright` package. They start local
ephemeral HTTP servers with synthetic responses, never an OpenProxy instance.
They explicitly request the Chromium sandbox and do not disable it. These
are narrow interaction checks, not the visual parity suite. The Endpoint smoke
requires a full Chrome/Chromium build that enforces blocked site-data settings;
headless-shell builds can ignore that preference.

To run the slice, serve the build at the origin root with SPA fallback for the
implemented deep links, and forward `/api/` to a **separately isolated test
server** on the same origin. The reference translation catalogs are included
at `/i18n/literals/`. The app uses same-origin cookies, not a mock
authentication implementation. Do not point this preview at production data
or credentials.

Settings uses the existing theme and locale controls, authentication and
network settings, observability, database export/import, shutdown and logout
APIs. Database export and import retain the server's password reauthentication;
the browser does not bypass or replace that check.

## Reference assets and known gaps

`assets/dashboard.BZnXx_Nc.css` and its referenced
`public/_astro/material-symbols-outlined.CqIkmgaP.woff2` are exact copies from
the existing `web/dist` build; `public/favicon.svg` is the same favicon with an
extraneous trailing blank line removed. The
font's original URL path is kept because the stylesheet references it by
absolute URL. The CSS is bundled without further minification and injected into
the initial HTML head. The stylesheet also imports Inter, Instrument Serif,
and JetBrains Mono from Google Fonts, exactly
as the current build does; rendering when those fonts are unavailable needs to
be considered in visual tests. These reference assets are included to keep
this slice independently buildable; refresh them deliberately from the Astro
build if the reference changes.

`public/i18n/literals/` contains unchanged copies of the reference catalogs in
`web/public/i18n/literals/` for all non-English supported locales; English uses
the original strings. The release build minifies the JSON without changing its
values. Refresh the copies deliberately when the reference catalogs change.

`src/browser/provider_data.json` is the list and chooser projection of
`web/src/shared/constants/providers.ts`, including category order, card fields,
free-tier membership, and the canonical rate-limit labels. The separate
`src/browser/provider_sites.json` contains the chooser's API-key and website
links. The provider PNGs in
`public/providers/` are unchanged copies of the icons used by those categories
and compatible cards. `tests/provider-metadata.cjs` checks the snapshot and
icon bytes and links against the reference (and flags new hidden, non-LLM,
deprecated, or cookie metadata that requires code changes). It also checks the
chooser's changing search-count text against the catalogs: that text is skipped
by the translator because none of the current catalogs translates its literal
text runs. Refresh the snapshots deliberately when the reference registry,
icons, or these catalog entries change; the standalone build does not depend on
TypeScript or the Astro application.

`src/browser/provider_aliases.json` contains the reference registry's
non-identity storage aliases (for example `kilocode` → `kc`); identity aliases
fall back to the provider ID. The drift check compares every registry alias.
The detail page reads the existing catalog, connections, custom models,
legacy aliases and disabled IDs. Standard providers support model copy,
favorites, test, add/remove, individual disable/restore, disable/activate all,
and the persisted free-only filter. Compatible nodes support manual add/remove,
import from the first active connection, copy and test using the node ID for
storage and the node prefix for display.
Details resolve UUID and legacy prefixed node IDs using the persisted compatible
type; known built-in providers retain precedence over node IDs.
Their detail page supports editing node configuration, explicit credential
checks and confirmed deletion using the existing node APIs. Deleting a node
does not delete its connections or custom models.
Connection add/edit, status, priority ordering, selection, bulk deletion, proxy
assignment and paced one-by-one testing use the existing connection APIs.
Codex OAuth and GLM API-key connections also support the persisted auto-ping
setting. The existing full-map settings API cannot prevent simultaneous writes
from different tabs from overwriting one another.
Codex detail also supports its signup link and bulk auth.json import. Codex,
OpenCode Go, GLM and Claude expose their advertised-context settings; the
setting re-reads and merges the saved map before writing, but cannot prevent
concurrent cross-tab full-map writes.
Editing never pre-populates a stored API key; a blank replacement leaves it
unchanged. Device-code OAuth and manual authorization-code exchange use the
existing OAuth endpoints. The synchronized thinking selectors only append a
supported suffix to display/copy; they do not alter model mutation IDs. Failed
bulk deletes remain visible and selected. Free-tier, notice and auth-mode
metadata are checked snapshots of the reference provider registry. Required model reads that fail do
not become an empty saved configuration or expose bulk mutation controls; the
user can retry. Optional live discovery is merged when available and cannot
block those saved rows.

The callback relays through same-origin BroadcastChannel and storage, plus
`postMessage` to an opener at the callback's exact origin. Automatic OAuth
completion requires the active nonempty state and, when supplied, the matching
provider; message origins must exactly match the app or its active redirect
origin. For Cline's state-less callback, the redirect URI instead carries a
cryptographically random, per-attempt nonce; the backend's original state is
still used for the exchange. Whether the live Cline provider preserves that
redirect URI query has not been verified.

For supported same-origin return paths, the dashboard retains a popup handle
but clears its opener **before** navigating to the authorization page. It
polls the handle after the popup returns to the exact app origin and callback
path, even if both channel and storage are unavailable. Following successful
exchange, it closes only the matching consumed popup; invalid callbacks and
failed exchanges remain available for manual copy. A blocked or severed popup
handle, or a callback on a different origin (for example `localhost` versus
`127.0.0.1`), requires manual URL copy/paste. A callback with an actual opener
closes two seconds after a successful code, as in the reference. The standalone
page renders only after WASM starts; it cannot reproduce Astro's pre-WASM first
paint.

Detail is **not reference-complete or pixel-perfect**: some OAuth modal details
(including copy buttons) remain to migrate.
Local OAuth proxy ownership is coordinated
within this browser application only: the backend stop endpoint is
provider-wide, so other tabs/processes and an unconfirmed stop after a
transport failure cannot be isolated by this client alone. A pending stop
produces a retryable error after three seconds without releasing the start
fence. If the stop transport fails or returns a non-success response without
acknowledgement, that page stays fenced; starting again after a page reload
cannot be guaranteed safe from a delayed provider-wide stop. A start already
in flight can also take effect after all dialogs have closed and revive the
listener; the client sends another best-effort stop when that request settles,
but cannot guarantee the listener stays stopped before then or if the response
never arrives. Some proxy, capability, error, loading and
confirmation states differ. Manual compatible model layout is partial. The model controls deliberately
wait for the required catalog/custom/alias/disabled reads rather than rendering
partial mutable state. No new backend model or discovery contract was added.

The current reference cookie-provider registry is empty; that section would
require implementation if it is populated.

The vendored Material Symbols Outlined font originates from the
`material-symbols` npm package v0.46.0 and is licensed under Apache-2.0; see
[`LICENSE.material-symbols`](LICENSE.material-symbols).

Theme is read from the existing `theme` localStorage setting in Rust before
the Dioxus app starts. Unlike the Astro inline script, WASM cannot apply that
setting synchronously before the first browser paint; a flash before WASM
startup is possible. The dashboard skeleton is also rendered by WASM, so it
does not provide Astro's pre-WASM first-paint coverage. Login and the dashboard
share the reference's literal-text translation behavior: missing catalog entries
and input placeholder attributes remain untranslated, and in-place changes to
an existing text node are not observed automatically. No claim of pixel parity,
production compatibility, or completeness of the dashboard is made.
