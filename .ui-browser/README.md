# Isolated dashboard browser fixture

This is a **local, synthetic-only** test environment, separate from the
production Compose project. It uses `target/debug/openproxy` and `web/dist` as
read-only bind mounts. The backend serves that Astro build with `--web-dir`;
the fixture-only proxy exposes its UI and API at the same internal origin,
`https://app:4623`. No service publishes a port, uses host networking, mounts a production
volume, or receives production secrets. The Compose network is internal with
an isolated gateway, so the bridge has no host gateway address. It is not a
general security boundary against a hostile host or Docker administrator.

The backend has a fresh tmpfs data directory and the deliberately public test
password `synthetic-browser-only`; its HTTPS session cookie is marked Secure.
Its data disappears when its container
stops. Never enter real credentials, connect accounts, or capture private data.
The browser uses a new tmpfs profile per MCP process and removes it on exit.
MCP speaks stdio; Chrome DevTools MCP 1.11.0 connects to Chrome over a pipe,
not an exposed CDP port. The browser runs as UID/GID 1000 with Chrome's
sandbox and TLS verification enabled and no `SYS_ADMIN` capability. Its
browser-only seccomp profile makes the narrow namespace allowances documented
below; the browser's setuid sandbox helper is not setuid. The backend and proxy
keep Docker's default profile. A host that still
prevents Chrome's sandbox from starting is a blocker: **do not use
`--no-sandbox`, privileged mode, `SYS_ADMIN`, an
unconfined seccomp profile, or insecure certificate flags as a workaround**.

## Prerequisites and versions

- Linux x86-64, Docker with isolated bridge gateway support (verified on
  Docker 29.4.3), Compose, Python 3, and a working sandbox-capable
  Docker/kernel configuration. The test binary must be Linux x86-64 and
  compatible with Debian 13's glibc. Rebuild it separately when needed;
  this fixture does not build or tag the production image.
- An existing `target/debug/openproxy` and built Astro `web/dist/index.html`.
- The browser image pins Chrome for Testing 155.0.8059.79 (SHA-256 checked),
  `chrome-devtools-mcp` 1.11.0, Playwright 1.63.0, ImageMagick
  7.1.1.43+dfsg1-1+deb13u12, the Node base digest, a Debian package snapshot,
  and the npm lockfile. Debian's signed CA package is bootstrapped before
  fetching the snapshot over verified HTTPS. Building needs network access;
  the running fixture does not need internet access.

## Build, start, and smoke test

From the repository root, use **only** this Compose file and project name:

```sh
docker compose --project-name openproxy-ui-browser-r1 --project-directory .ui-browser -f .ui-browser/compose.yaml build browser
docker compose --project-name openproxy-ui-browser-r1 --project-directory .ui-browser -f .ui-browser/compose.yaml up -d --no-deps --pull never backend app
python3 .ui-browser/smoke.py
opencode mcp list
```

The smoke performs the actual MCP initialize, catalog, browser launch, page
navigation, synthetic login, authenticated dashboard verification, and PNG
capture. It reports the final URL and screenshot dimensions. A unique PNG is
written under ignored `.ui-browser/artifacts/`; remove only your own unwanted
synthetic captures manually. `opencode mcp list` demonstrates connection and
discovery only: it does **not** launch Chrome, so it cannot prove the sandbox or
screenshot works. `smoke.py` must succeed for that evidence. In an isolated
OpenCode 2.0.29 service, an immediate first `mcp list` returned no servers
while the service was still initializing; a later invocation showed the
project-local server connected.

## Project-local OpenCode V2 setup

The ignored root `opencode.jsonc` is local machine configuration and must not
be committed. Merge the following entry into an existing file without replacing
unrelated settings (or create the file if absent):

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "chrome-devtools-local": {
        "type": "local",
        "command": [".ui-browser/mcp.sh"],
        "cwd": ".",
        "timeout": { "startup": 60000, "catalog": 30000, "execution": 600000 }
      }
    }
  }
}
```

Run OpenCode from the repository root. After building the image and starting
the app, use the server's `new_page` tool with `https://app:4623/dashboard` or
`https://app:4623/login`. Screenshots can be returned in the MCP response. A
`filePath` passed to an MCP tool is **inside the disposable browser container**,
not on the host; use an inline response or the smoke script to save it locally.
The wrapper runs `docker compose run --rm --no-deps --pull never -T` and will
not rebuild an image or start the app automatically.

The fixture proxy's SSE disconnect handling has a synthetic regression check
(requires Docker Compose 2.24.4 or later):

```sh
sh .ui-browser/tests/stream-cleanup.sh
```

To verify that the proxy serves both Latin and Cyrillic glyphs from the pinned
font files, rather than silently falling back to a system font:

```sh
docker compose --project-name openproxy-ui-browser-r1 --project-directory .ui-browser -f .ui-browser/compose.yaml run --rm --no-deps --pull never -T \
  -v "$PWD/.ui-browser/tests/font-subsets.test.mjs:/test/font-subsets.test.mjs:ro" \
  browser node --test /test/font-subsets.test.mjs
```

To stop the isolated app when finished, explicitly run:

```sh
docker compose --project-name openproxy-ui-browser-r1 --project-directory .ui-browser -f .ui-browser/compose.yaml stop app backend
```

No command here removes volumes automatically. Stopping the backend discards its
tmpfs data. If a browser run fails, capture the error; a successful MCP catalog
is not proof that the sandbox started. Do not weaken browser or container
security to make a failing smoke pass.

Docker does not update an existing network's gateway mode in place. After a
network configuration change, disconnect the local MCP and run `down` for this
explicit test project before starting it again; this discards its tmpfs data.

## Browser sandbox profile

`.ui-browser/seccomp-chrome.json` is based on the exact official [Moby Docker
29.4.3 default profile](https://github.com/moby/moby/blob/docker-v29.4.3/vendor/github.com/moby/profiles/seccomp/default.json)
(Apache-2.0; unmodified source SHA-256
`536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74`).
The license is included in `.ui-browser/LICENSE.seccomp`. This fixture's
modified profile adds only these browser-container rules:

- `unshare` when argument 0 is exactly `CLONE_NEWUSER` (`0x10000000`).
- `clone` when `(flags & 0x1e020000) == 0x10000000`: a new user namespace,
  optionally with PID and/or network namespaces, but not mount, UTS, IPC, or
  cgroup namespaces.
- `clone` when `(flags & 0x7e020000) == 0x20000000`: a new PID namespace
  without any of the other masked namespace flags, for Chrome's nested zygote.

The default profile's other rules, including the `clone3` behavior, remain
unchanged. This is a security relaxation versus Docker's default, scoped to the
disposable browser container; it does not change the daemon or host policy.
Chrome's `chrome://sandbox` reports namespace, PID, network and seccomp-BPF
sandboxing on the validated host. Revalidate the profile against the runtime
and Chrome versions if either changes.

## Synthetic HTTPS and fonts

Chrome preloads HSTS for the `.app` TLD, including the fixture hostname `app`.
The image therefore generates a fixture-only CA constrained to `DNS:app` and
an `app` leaf certificate; it discards the CA private key in the build layer.
The browser entrypoint trusts only that CA in its disposable per-user NSS
database. The retained leaf private key is public synthetic test material, not
a production credential. No certificate verification is disabled. The proxy is
HTTPS-only and is unreachable from the host except through Docker's internal
network; the browser and API stay same-origin.

Because the network is internal, the proxy replaces only the dashboard's
Google Fonts CSS import with `/_astro/__ui_browser/fonts.css`. The image fetches
and SHA-256 verifies the original WOFF2 subsets and preserves their Unicode
ranges for Inter v20, Instrument Serif v5, and JetBrains Mono v24. The browser
loads them from the app origin without external font requests; scripts not
covered by Google Fonts use the browser's normal fallback. Material Symbols
remains the Astro build's own bundled asset. These fonts are open source under the SIL
Open Font License; upstream license texts are [Inter](https://github.com/google/fonts/blob/main/ofl/inter/OFL.txt),
[Instrument Serif](https://github.com/google/fonts/blob/main/ofl/instrumentserif/OFL.txt),
and [JetBrains Mono](https://github.com/google/fonts/blob/main/ofl/jetbrainsmono/OFL.txt).
The pinned license texts are also included in the image under `/opt/ui-browser/fonts/`.

## Visual parity test integration

The image includes the executable and tools expected by
`tests/ui_parity.browser.test.mjs`. The test must use
`https://app:4623` as its allowed app origin; fixture font URLs are already
under its `/_astro/` asset allowance. Mount the test read-only and use an
ignored, synthetic-only output directory writable by UID 1000. For example,
after preparing `.ui-browser/artifacts/parity-run` with those permissions:

```sh
docker compose --project-name openproxy-ui-browser-r1 --project-directory .ui-browser -f .ui-browser/compose.yaml run --rm --no-deps --pull never -T \
  -v "$PWD/tests/ui_parity.browser.test.mjs:/test/ui_parity.browser.test.mjs:ro" \
  -v "$PWD/.ui-browser/artifacts/parity-run:/artifacts" \
  -e RUN_UI_PARITY_TESTS=1 -e UI_PARITY_MODE=record -e UI_PARITY_SCREEN=login-ready \
  -e UI_REFERENCE_DIR=/artifacts/reference -e UI_ARTIFACT_DIR=/artifacts/results \
  -e DASHBOARD_PLAYWRIGHT_MODULE=/opt/ui-browser/node_modules/playwright/index.mjs \
  -e DASHBOARD_CHROMIUM_EXECUTABLE=/opt/chrome-linux64/chrome \
  browser node --test /test/ui_parity.browser.test.mjs
```

Recording is explicit and refuses to replace existing references unless the
test's explicit replacement option is set. It pins the Chrome version and
launch flags in `source.json`; incompatible references must be recorded again.
The test disables Chrome's partial rasterization because it produced
nondeterministic rounded-edge pixels even for Astro compared with itself. It
still requires exact pixel equality, without masks or tolerance. Do not record
over approved references or point this fixture at real data.

For candidate comparison, use a **different** isolated Compose project and a
fresh Dioxus build directory; do not overlay it onto the Astro directory because
old HTML files take precedence over a Dioxus SPA entrypoint. Copy the reviewed
synthetic reference PNGs and `source.json` to `tests/ui/reference/` first.
Then, from the repository root:

```sh
export OPENPROXY_UI_WEB_DIR="$PWD/frontend/target/dx/openproxy-dashboard/release/web/public"
project="openproxy-ui-dioxus-$(date +%s)-$$"
docker compose --env-file /dev/null --project-name "$project" --project-directory .ui-browser -f .ui-browser/compose.yaml -f .ui-browser/tests/silent-backend.compose.yaml up -d --no-deps --pull never backend app
docker compose --env-file /dev/null --project-name "$project" --project-directory .ui-browser -f .ui-browser/compose.yaml -f .ui-browser/tests/silent-backend.compose.yaml run --rm --no-deps --pull never -T \
  -v "$PWD/tests/ui_parity.browser.test.mjs:/test/ui_parity.browser.test.mjs:ro" \
  -v "$PWD/tests/ui/reference:/references:ro" \
  -v "$PWD/.ui-browser/artifacts/parity-run:/artifacts" \
  -e RUN_UI_PARITY_TESTS=1 -e UI_PARITY_MODE=compare -e UI_PARITY_SCREEN=login-ready \
  -e UI_REFERENCE_DIR=/references -e UI_ARTIFACT_DIR=/artifacts/results \
  -e DASHBOARD_PLAYWRIGHT_MODULE=/opt/ui-browser/node_modules/playwright/index.mjs \
  -e DASHBOARD_CHROMIUM_EXECUTABLE=/opt/chrome-linux64/chrome \
  browser node --test /test/ui_parity.browser.test.mjs
docker compose --env-file /dev/null --project-name "$project" --project-directory .ui-browser -f .ui-browser/compose.yaml -f .ui-browser/tests/silent-backend.compose.yaml run --rm --no-deps --pull never -T \
  -v "$PWD/tests/dioxus_login_backend.browser.test.mjs:/test/dioxus_login_backend.browser.test.mjs:ro" \
  -e RUN_DIOXUS_BACKEND_TEST=1 -e DIOXUS_BACKEND_OUTPUT_SUPPRESSED=1 \
  -e DASHBOARD_PLAYWRIGHT_MODULE=/opt/ui-browser/node_modules/playwright/index.mjs \
  -e DASHBOARD_CHROMIUM_EXECUTABLE=/opt/chrome-linux64/chrome \
  browser node --test /test/dioxus_login_backend.browser.test.mjs
docker compose --env-file /dev/null --project-name "$project" --project-directory .ui-browser -f .ui-browser/compose.yaml -f .ui-browser/tests/silent-backend.compose.yaml down
unset OPENPROXY_UI_WEB_DIR project
```

Record mode verifies Astro's component identity; compare mode requires the
Dioxus loader, so comparing the reference to itself is rejected. Before the
full migration, select only implemented states with `UI_PARITY_SCREEN`. The
real-backend test needs a fresh backend and checks the Codex advertised context
setting, a synthetic API key and provider connections after page reload. Its
auto-ping check uses an inactive synthetic GLM connection, so it cannot start
the worker or contact a provider; it never reads or prints the generated key
value. The override suppresses the synthetic key printed at backend startup.

The provider-model discovery test needs a **fresh** disposable backend. Its
startup generates a synthetic API key and writes it to stderr, so the test-only
override redirects backend output and disables Docker logging. From the repo
root, with a Dioxus candidate built using `frontend/scripts/build.sh`:

```sh
export OPENPROXY_UI_WEB_DIR="$PWD/frontend/target/dx/openproxy-dashboard/release/web/public"
project="openproxy-ui-models-$(date +%s)-$$"
docker compose --env-file /dev/null --project-name "$project" --project-directory .ui-browser -f .ui-browser/compose.yaml -f .ui-browser/tests/silent-backend.compose.yaml up -d --no-deps --pull never backend app
docker compose --env-file /dev/null --project-name "$project" --project-directory .ui-browser -f .ui-browser/compose.yaml -f .ui-browser/tests/silent-backend.compose.yaml run --rm --no-deps --pull never -T \
  -v "$PWD/tests/dioxus_provider_models_backend.browser.test.mjs:/test/dioxus_provider_models_backend.browser.test.mjs:ro" \
  -e RUN_DIOXUS_BACKEND_TEST=1 -e DIOXUS_BACKEND_OUTPUT_SUPPRESSED=1 \
  -e DASHBOARD_PLAYWRIGHT_MODULE=/opt/ui-browser/node_modules/playwright/index.mjs \
  -e DASHBOARD_CHROMIUM_EXECUTABLE=/opt/chrome-linux64/chrome \
  browser node --test /test/dioxus_provider_models_backend.browser.test.mjs
docker compose --env-file /dev/null --project-name "$project" --project-directory .ui-browser -f .ui-browser/compose.yaml -f .ui-browser/tests/silent-backend.compose.yaml down
unset OPENPROXY_UI_WEB_DIR project
```

The test creates, disables, reloads and restores a synthetic custom model,
checking `/v1/models` at each step. It does not contact a provider or verify
database durability across backend restarts; Compose `down` removes the tmpfs.
