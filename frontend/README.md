# Dioxus dashboard migration slice

This is a separate, browser-only Rust/WASM application. It implements the
`/login` route against the existing same-origin authentication API; it does
not replace the Astro frontend or alter how OpenProxy is packaged or served.
Other dashboard routes are not implemented here. Login navigations use full
browser redirects to `/dashboard` or `/login`, as the current frontend does.

## Build and test

Use Rust 1.98.1 (the tested frontend toolchain) with the `wasm32-unknown-unknown`
target and **Dioxus CLI 0.7.10** (for example,
`cargo install dioxus-cli --version '=0.7.10' --locked`). Then, from this
directory:

```sh
cargo test --locked
dx build --web --release --cargo-args=--locked
```

The independently locked build is emitted under
`target/dx/openproxy-dashboard/release/web/public/`. The Dioxus-generated WASM
and JavaScript bridge filenames may include content hashes and may change on
rebuild; they are build artifacts and are ignored, not source files. The CLI
may download matching build tools to its own user cache. Incremental `dx`
builds can retain older, unreferenced hashed assets (and old files formerly
in `public/`) in this output directory. A future packaging step must stage a
clean build output rather than treating this cache as a production-ready tree.

An optional isolated behavioral smoke test is available after the release build:

```sh
# Install Playwright separately and provide a sandbox-capable Chromium if needed.
CHROME_BIN=/path/to/chromium node tests/login-smoke.cjs
```

The smoke test requires Node and the `playwright` package. It starts a local
ephemeral HTTP server and intercepts authentication requests with synthetic
responses; it never connects to an OpenProxy instance. It explicitly requests
the Chromium sandbox and does not disable it. This is a narrow interaction
check, not the visual parity suite.

To run the slice, serve the build at the origin root with SPA fallback for the
`/login` deep link, and forward `/api/` to a **separately isolated test server**
on the same origin. The app uses same-origin cookies, not a mock authentication
implementation. Do not point this preview at production data or credentials.
The build is not a complete dashboard: a successful login redirects to the
existing `/dashboard`, which this slice does not serve.

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

The vendored Material Symbols Outlined font originates from the
`material-symbols` npm package v0.46.0 and is licensed under Apache-2.0; see
[`LICENSE.material-symbols`](LICENSE.material-symbols).

Theme is read from the existing `theme` localStorage setting in Rust before
the Dioxus app starts. Unlike the Astro inline script, WASM cannot apply that
setting synchronously before the first browser paint; a flash before WASM
startup is possible. Login behavior and appearance have not yet been verified
against the reference in an isolated browser, and no claim of pixel parity,
production compatibility, or completeness of the dashboard is made here.
