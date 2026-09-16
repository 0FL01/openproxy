# Contributing to OpenProxy

**OpenProxy** is a Rust AI proxy router — single binary on `127.0.0.1:4623`, OpenAI-compatible, 40+ providers, embedded Astro dashboard. This guide makes contributions systematic, not arbitrary.

> **Single source of truth for daily workflow:** run `./scripts/dev.sh` — it builds the backend and starts the server. Tests are explicit cargo commands (see [Testing](#testing)).

Related docs:
- **Git conventions (branch naming, commit messages, atomic commits, PR hygiene):** [`docs/git-conventions.md`](docs/git-conventions.md)
- **Agent intelligence brief:** [`AGENTS.md`](AGENTS.md) — architecture, secrets policy, schema stability
- **Pipeline order & intentional behavior:** [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)

---

## Table of Contents
1. [Prerequisites](#prerequisites)
2. [Getting Started](#getting-started)
3. [Project Layout](#project-layout)
4. [Development Workflow](#development-workflow)
5. [Coding Standards](#coding-standards)
6. [Testing](#testing)
7. [Tasks](#tasks)
8. [Commits, Branches & PRs](#commits-branches--prs)
9. [Secrets & Local Config — Never Commit](#secrets--local-config--never-commit)
10. [Reporting Bugs & Requesting Features](#reporting-bugs--requesting-features)
11. [Release Process](#release-process)
12. [License & Conduct](#license--conduct)

---

## Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| Rust | 1.76+ (`stable`) | `rustup update` — needs `rustfmt` + `clippy` components |
| Node | 20.3+ | Required for dashboard build |
| pnpm | 10.33.2 | `corepack enable && corepack prepare pnpm@10.33.2 --activate` |
| cargo / git / curl / jq | — | `jq` needed for `--robot` JSON parsing |

Optional: `fuser`, `ss`, `gh` CLI.

## Getting Started

```bash
git clone https://github.com/quangdang46/openproxy.git
cd openproxy

# quick is the default: cargo build + web build + quick tests + start detached on :4625
./scripts/dev.sh

# verify
curl -sf http://127.0.0.1:4625/health
./target/debug/openproxy --data-dir ~/.openproxy-dev --robot server status
./target/debug/openproxy --data-dir ~/.openproxy-dev --robot doctor
```

Common variants (see `AGENTS.md` and `scripts/dev.sh`):

```bash
./scripts/dev.sh            # build + run foreground (Ctrl+C to stop)
./scripts/dev.sh build      # only build, don't run
./scripts/dev.sh detach     # build + run detached
PORT=4626 ./scripts/dev.sh  # custom port
```

Live dashboard iteration without rebuilding the Rust binary:

```bash
pnpm --dir web dev               # Astro dev on :4624 (proxies API to :4625)
# in another terminal:
cargo run -- --port 4625 --data-dir ~/.openproxy-dev --dashboard-sidecar-url http://127.0.0.1:4624
```

## Project Layout

```
openproxy/
├── src/                 # Rust — axum, hyper, SQLite WAL, encrypted columns
│   ├── core/            # model parsing, format translation, executor trait
│   ├── server/api/      # /v1, /api, provider_models, chat
│   └── cli/             # provider apply, schema, doctor
├── web/                 # Astro 4 + React 19 + Tailwind — built to web/dist
│   └── src/             # dashboard components, provider constants
├── scripts/dev.sh       # THE dev entrypoint (build+test+run)
├── docs/                # architecture, state, git-conventions
├── .github/workflows/   # CI: web (astro check+build) → rust (fmt+clippy+tests)
├── tests/               # provider_baseline.json, verify_no_regression.mjs
└── Cargo.toml / web/package.json
```

The Rust binary embeds `web/dist` via `rust-embed` at compile time — if you change dashboard code, run `pnpm --dir web run build` or just `./scripts/dev.sh` again.

## Development Workflow

1. **Code** — follow [Coding Standards](#coding-standards). Keep changes focused; one logical change per commit.
2. **Build + test + run** — always via `scripts/dev.sh`:
   ```bash
   ./scripts/dev.sh build  # fast feedback before starting server
   ./scripts/dev.sh         # build + run foreground
   ```
3. **Verify** — `curl /health`, `openproxy --robot doctor`, smoke `cargo test -p openproxy --lib parity_tests`.
4. **Push to main** — work directly on `main`; create a branch only when explicitly requested.

If dashboard changes appear "missing" at runtime, you forgot `pnpm --dir web run build` — `scripts/dev.sh` does it for you.

## Coding Standards

### Rust
- `cargo fmt --all -- --check` must pass — do not hand-format. CI enforces.
- `cargo clippy --all-targets --all-features` must pass — zero warnings. The workflow runs it without `-D warnings` but all clippy lints are treated as errors.
- No `unwrap()`-heavy paths in hot code; prefer `Result` / typed errors (`thiserror`).
- No `as any` / `@ts-ignore` equivalent — fix types, don't suppress.
- Prefer existing crates over new dependencies. Justify any new dep in the PR description.
- 250 LOC ceiling per module — split oversized files.

### TypeScript / Web
- Astro `pnpm exec astro check` is advisory today (see CI `|| true`) but fix new type errors you introduce.
- `pnpm --dir web run build` must succeed. The Rust `build.rs` requires `web/dist`.

### General
- Small, focused changes over large refactors.
- Never suppress type errors or leave TODOs without tracking.
- Fix minimally when fixing bugs — don't refactor in the same commit.

## Testing

Local gate mirrors CI (which runs fmt + clippy + `cargo test --lib` on Linux only):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --lib --all-features
```

Integration tests (`tests/`, 61 targets) run only for affected surfaces: `cargo test --test <name>`.
Touched `tests/common/`, `server/api`, auth, DB, or pools → run all related integration targets, not just one.

| Command | What it runs |
|---------|--------------|
| `cargo test -p openproxy --lib provider_models -- --nocapture` | 13 provider-model parsing + discovery tests |
| `cargo test --test providers_api import_catalog -- --nocapture` | 4 import catalog tests |
| `cargo test -p openproxy --lib parity_tests -- --nocapture` | stream_flags smoke |
| `cargo test -p openproxy --lib -- --test-threads=1` | full 1727-test lib suite |
| `cargo test --test providers_api -- --test-threads=1` | providers API (full) |
| `pnpm --dir web exec astro check` | dashboard typecheck |

Add regression coverage for provider model changes in `tests/provider_baseline.json` (+ `tests/verify_no_regression.mjs`).

Golden-snapshot translator coverage lives in `src/core/translator/tests.rs`.

**Evidence rule:** a task is not done until `cargo fmt` + `cargo clippy` + relevant tests are green — CI enforces the same.

## Tasks

Use beads or GitHub issues for own product tasks only.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) before touching provider-specific logic.

## Commits, Branches & PRs

**Full rulebook:** [`docs/git-conventions.md`](docs/git-conventions.md) — read it before your first commit.

Quick summary (Conventional Commits):

```
<type>(<scope>): <imperative subject>

[optional body — what & why, not how]
[optional footer — Fixes #123, BREAKING CHANGE:]
```

- **Types:** `feat`, `fix`, `docs`, `chore`, `test`, `refactor`, `perf`, `ci`, `build`, `revert`
- **Scope:** `providers`, `dashboard`, `executor`, `api`, `agents`, `dev`, etc.
- **Branches:** `feat/…`, `fix/…`, `docs/…`, `chore/…`, `refactor/…`, `test/…` (mirrors commit type)
- **Atomic commits:** one logical change per commit, bisectable, independently revertible. Never `git add .` blindly.
- **Before each commit:** `git status`, `git diff --cached`, `cargo fmt`, `cargo clippy`, relevant tests — all green.

PRs use `.github/pull_request_template.md` — fill in summary, test plan, and risk. Keep PRs ≤ ~400 lines where possible; split large work into stacked PRs.

## Secrets & Local Config — Never Commit

This is a hard rule (also in `AGENTS.md`):

- **Never commit:** `opencode.json`, `opencode.jsonc`, `.env`, `.env.*`, `*.pem`, `~/.openproxy/db.json`, `~/.openproxy/admin.key`, any file containing `sk-`, `Bearer`, `refresh_token`, or live `provider_specific_data`.
- `opencode.json` is **machine-local agent config** — keep untracked. Real secrets live in SQLite (`openproxy.sqlite`, encrypted) + `OPENPROXY_API_KEY` env.
- Before `git add`/`commit`: `git status` → `git diff --cached` → `git restore --staged <file>` if it contains secrets → add to `.gitignore` → `git check-ignore -v <file>` to verify.
- If a secret is accidentally committed: **rotate immediately** and purge history (`git filter-repo` or BFG) — don't just revert.

`.gitignore` already excludes `opencode.json`, `.env*`, `*.pem`, `.openproxy/`, `admin.key`.

## Reporting Bugs & Requesting Features

- **Search first:** `gh issue list` / `gh search issues` to avoid duplicates.
- **Use the issue templates** (`.github/ISSUE_TEMPLATE/`): Bug Report / Feature Request. Fill in repro steps, expected vs actual, `openproxy --version`, and whether you can reproduce after `cargo test --lib --all-features`.
- **For provider bugs:** mention the provider, model, repro steps, expected vs actual.

## Release Process

- Version in `Cargo.toml` (`0.2.0`). Tags `v*` trigger `.github/workflows/release.yml` (GHCR + GitHub Release).
- CI must be green on `main` before tagging.
- `install.sh` / `install.ps1` pull from the same GitHub release — keep them in sync when bumping the binary.

## License & Conduct

- **License:** MIT — see [LICENSE](LICENSE).
- Be respectful, assume good intent, and keep discussion technical. No AI slop — code should be indistinguishable from a senior engineer's.

---

**New contributor?** Start with `./scripts/dev.sh` → open `http://127.0.0.1:4625/dashboard/providers` → add one API-key provider → `curl http://127.0.0.1:4625/v1/models -H "Authorization: Bearer $OPENPROXY_API_KEY"` → read `src/core/translator/` for format translation.

Questions? Open a Discussion or an issue with `type: question`.
