# Dioxus dashboard migration slice

This is a separate, browser-only Rust/WASM application. It implements `/login`,
the shared dashboard shell and Endpoint page at `/`, `/dashboard`, and
`/dashboard/endpoint`, and the list at `/dashboard/providers` against the
existing same-origin APIs. It does not replace the Astro frontend or alter how
OpenProxy is packaged or served. Provider detail and new-provider pages and
other dashboard pages are not implemented in this slice; their links remain
ordinary browser navigations, not substitute pages. Login and logout use full
browser redirects, as the current frontend does.

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

`src/browser/provider_data.json` is the list-specific projection of
`web/src/shared/constants/providers.ts`, including category order, card fields,
free-tier membership, and the canonical rate-limit labels. The provider PNGs in
`public/providers/` are unchanged copies of the icons used by those categories
and compatible cards. `tests/provider-metadata.cjs` checks the snapshot and
icon bytes against the reference (and flags new hidden, non-LLM, or cookie
categories that require code changes). Refresh both deliberately when the
reference registry or icons change; the standalone build does not depend on
TypeScript or the Astro application.

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
