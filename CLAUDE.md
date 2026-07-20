---
description: "Chelix engineering guide for Claude/Codex agents: Rust architecture, testing, security, and release workflows"
alwaysApply: true
---

# CLAUDE.md

Chelix engineering guide for Rust architecture, testing, security, and release workflows.
All code must have tests with high coverage. Always check for security.

## Core Development Rules

**All new functionality must be implemented simply, without hidden errors or fallbacks, in an architecturally correct manner, without legacy code, backward compatibility, or any form of "architectural garbage."**

**Hidden defaults are forbidden for any errors. All errors must be explicitly propagated to logs and must result in a refusal to provide service so that the problem can be detected and fixed as quickly as possible, without hidden scenarios.**

**All refactorings require removing previously created Claude garbage in the form of fallbacks within the affected scope and bringing the codebase up to the correct standard.**

**Examples of correct service behavior:**

- **An environment variable with an invalid value is passed at service startup — refuse to load without silently applying a default value.**
- **The sandbox type is set to Docker, but its socket is unavailable — refuse service.**
- **A sandbox image cannot be built while the sandbox is enabled in the service configuration — refuse service.**
- **A user edits an agent configuration through the UI and the configuration becomes invalid — show an error in the UI without overwriting the old agent configuration.**
- **An unknown or obsolete parameter is specified in the configuration — refuse to load the service.**
- **A request to an LLM provider API fails because the supplied parameters are incompatible — report the error in the UI and logs without hidden use of anything other than what was explicitly specified in the request.**
- **A token counter is used inside the agent loop and its indicator is shown in the UI — the UI must receive the real counter values from the backend directly from the point where they are counted or applied, without fake duplication, guessing, or transformation.**
- **A request to an LLM provider API uses a specific model and reasoning level — exactly those values must be displayed in the UI request history, taken directly from the request rather than from defaults, invented values, or separately duplicated intermediate entities.**
- **An MCP server explicitly listed among an agent's tools fails to start — the agent loop must not start and must explicitly state why the configured agent tools cannot be made operational.**

**Allowed:**

- **Retrying external interactions, such as network requests or external commands, a controlled number of times, with every retry explicitly recorded in logs and the fact of the retries propagated to the UI.**
- **Pre-approved data migrations when there is an explicit business need. An extra unknown configuration parameter must never create migration garbage — it must unambiguously cause refusal.**

## Cargo Features

Enable new feature flags **by default** in `crates/cli/Cargo.toml` (opt-out, not opt-in):
```toml
[features]
default = ["foo", ...]
foo = ["chelix-gateway/foo"]
```

## Workspace Dependencies

Add new crates to `[workspace.dependencies]` in root `Cargo.toml`, reference with `{ workspace = true }`.
Never add versions directly in crate `Cargo.toml`. Use latest stable crates.io version.

## Config Schema and Validation

When adding/renaming fields in `ChelixConfig` (`crates/config/src/schema.rs`), also update
`build_schema_map()` in `crates/config/src/validate.rs`. New enum variants for string-typed
fields need updates in `check_semantic_warnings()`.

## Rust Style and Idioms

- Do not add implementation code to `mod.rs` or `lib.rs`. Keep those files for module wiring, exports, and crate setup, move real logic into dedicated sibling modules.
- Use traits for behaviour boundaries. Prefer generics for hot paths, `dyn Trait` for heterogeneous/runtime dispatch.
- Derive `Default` when all fields have sensible defaults.
- Use concrete types (`struct`/`enum`) over `serde_json::Value` wherever shape is known.
- **Match on types, never strings.** Only convert to strings at serialization/display boundaries.
- Prefer `From`/`Into`/`TryFrom`/`TryInto` over manual conversions. Ask before adding manual conversion paths.
- **DRY cross-crate types:** When two crates need the same enum/struct, define it once in the lower-level crate and re-export via `pub type Alias = other_crate::Type` from the higher-level one. Never duplicate enums across crates or round-trip through strings (`parse(&id.to_string())`) to convert between mirror types.
- Prefer streaming over non-streaming API calls.
- Run independent async work concurrently (`tokio::join!`, `futures::join_all`).
- Never use `block_on` inside async context.
- **Forbidden:** `Mutex<()>` / `Arc<Mutex<()>>` — mutex must guard actual state.
- Use `anyhow::Result` for app errors, `thiserror` for library errors. Propagate with `?`.
- **Never `.unwrap()`/`.expect()` in production.** Workspace lints deny these. Use `?`, `ok_or_else`, `unwrap_or_default`, `unwrap_or_else(|e| e.into_inner())` for locks.
- Use `time` crate (workspace dep) for date/time — no manual epoch math or magic constants like `86400`.
- Prefer `chrono` only if already imported in the crate; default to `time` for new code.
- Prefer crates over subprocesses (`std::process::Command`). Use subprocesses only when no mature crate exists.
- Prefer guard clauses (early returns) over nested `if` blocks.
- Prefer iterators/combinators over manual loops. Use `Cow<'_, str>` when allocation is conditional.
- Keep public API surfaces small. Use `#[must_use]` where return values matter.

### Tracing and Metrics

All crates must have `tracing` and `metrics` features, gated with `#[cfg(feature = "...")]`.
Use `tracing::instrument` on async functions. Record metrics at key points (counts, durations, errors).
See `docs/metrics-and-tracing.md`.

## Build Commands

```bash
cargo build                  # Debug build
cargo build --release        # Release build
cargo run / cargo run --release
```

## Web UI (TypeScript + Preact + Vite)

TypeScript/TSX source in `crates/web/ui/src/`, built with Vite to `crates/web/src/assets/dist/`.
CSS and static assets in `crates/web/src/assets/`. Release mode embeds via `include_dir!`.
Generated assets (`dist/`, `css/style.css`, `style.css`, `sw.js`) are gitignored.
Run `just build-web-assets` to generate them (requires Node.js).
A `build.rs` check warns (debug) or fails (release/embedded-assets) if they are missing.
See `docs/src/frontend.md` for the full architecture guide.

### Build Commands

```bash
cd crates/web/ui
npm run build          # Vite: TS/TSX → dist/
npm run build:css      # Tailwind: input.css → ../src/assets/css/style.css
npm run build:sw       # esbuild: src/sw.ts → ../src/assets/sw.js
npm run build:all      # All three above
npm run dev            # Vite watch mode (rebuilds on save)
npx tsc --noEmit       # Type check (strict, must be 0 errors)
```

**After changing TS/TSX files**, always:
1. `biome check --write crates/web/ui/src/`
2. `cd crates/web/ui && npm run build`
3. `cd crates/web/ui && npx tsc --noEmit`

### TypeScript Rules

- **File size limit: 1,500 lines**. Split large files into modules by domain.
  - Pages: extract sections/modals into `pages/sections/`, `pages/channels/`, `pages/chat/`, etc.
  - Utilities: extract sub-modules into sibling directories (`providers/`, `sessions/`, `ws/`).
  - Keep shared signals, types, and re-exports in the main file; move logic into sub-modules.
- All UI code is **TypeScript** with **JSX** (Preact). No HTM tagged templates.
- Add typed Props interfaces for all Preact components.
- Use `@preact/signals` with generic type parameters: `signal<string[]>([])`.
- Prefer typed interfaces over `Record<string, unknown>` — define concrete shapes where property access is known.
- Use `targetValue(e)` / `targetChecked(e)` from `typed-events.ts` for form event handlers.
- No `any` types — use `unknown` with type guards or specific interfaces.
- Use shared components from `components/forms/` (TextField, SaveButton, ListItem, Badge, TabBar, etc.).

### CSS Rules

- **Always use Tailwind classes** instead of inline `style="..."`.
- Reuse CSS classes from `components.css`: `provider-btn`, `provider-btn-secondary`, `provider-btn-danger`.
- Match button heights/text sizes when elements sit together.
- **Rebuild Tailwind** after adding new classes: `cd crates/web/ui && npm run build:css`.

### Adding Settings Nav Icons

Settings sidebar icons use `::before` pseudo-elements in `components.css`, **not** the `icon`
JSX property in the `sections` array. When adding a new settings section:

1. Create the SVG mask in `crates/web/src/assets/icons/masks/` with `fill="black"` (not `currentColor`)
2. Add `.icon-<name>` class in `crates/web/ui/input.css` under the mask-image icons section
3. **Also add** `.settings-nav-item[data-section="<id>"]::before` in `crates/web/src/assets/css/components.css`
   pointing to the SVG — without this the icon renders as a black square
4. The `icon: <span className="icon icon-<name>" />` in `SettingsPage.tsx` is a fallback only

### E2E Test Shims

E2E tests dynamically import individual JS modules (`js/state.js`, `js/helpers.js`, etc.).
With Vite bundling, these don't exist as standalone files. Shim files in `src/assets/js/`
proxy to `window.__chelix_modules` (populated by `app.tsx`). When adding new modules that
tests import, add a shim file and expose the module in `app.tsx`.

### Selection Cards

Use clickable cards (`.model-card`, `.backend-card` in `input.css`) instead of dropdowns for option selection.
States: `.selected`, `.disabled`, default. Badges: `.recommended-badge`, `.tier-badge`.

### Provider Config Storage

Provider keys in `~/.config/chelix/provider_keys.json` via `KeyStore` in `provider_setup.rs`.
When adding fields, update: `ProviderConfig` struct, `available()` response, `save_key()`.

### Server-Injected Data (gon pattern)

For server data needed at page load: add to `GonData` in `server.rs` / `build_gon_data()`.
TS side: `import * as gon from "./gon"` — use `gon.get()`, `gon.onChange()`, `gon.refresh()`.
Types in `crates/web/ui/src/types/gon.ts` mirror the Rust `GonData` struct.
Never inject inline `<script>` tags or build HTML in Rust.

### Event Bus

Server events via WebSocket: `import { onEvent } from "./events"`. Returns unsubscribe function.
Do **not** use `window.addEventListener`/`CustomEvent` for server events.

## API Namespace Convention

Each UI tab gets its own API namespace: REST `/api/<feature>/...` and RPC `<feature>.*`.
Never merge features into a single endpoint.

## Channel Message Handling

**Always respond to approved senders** — no silent failures. Send error/fallback messages
for LLM failures, transcription failures, unhandled message types. Access control via
allowlist/OTP flow.

## Adding Channels

When adding a new channel or extending one, follow `docs/channel-integration-checklist.md`.

Minimum bar before shipping:
- Settings reachable from the web UI, with onboarding coverage if the channel is offered there
- Advanced JSON config escape hatch for settings without dedicated HTML fields yet
- Prefer declarative channel field definitions that can drive both HTML forms and advanced JSON guidance
- Storage behavior explained clearly, web UI channel settings live in `data_dir()/chelix.db`, not `chelix.toml`
- Config template, validation, docs, and tests updated in the same PR
- No silent access-control failures, OTP and allowlist behavior must be user-visible

## Authentication Architecture

Password + passkey (WebAuthn) auth in `crates/gateway/src/auth.rs`, routes in `auth_routes.rs`,
middleware in `auth_middleware.rs`. Setup code printed to terminal on first run.
`RequireAuth` middleware protects `/api/*` except `/api/auth/*` and `/api/gon`.
`CredentialStore` persists argon2-hashed passwords, passkeys, API keys, sessions to JSON.

CLI: `chelix auth reset-password`, `chelix auth reset-identity`.

## Testing

```bash
cargo test --workspace --exclude chelix-embedding-service  # All macOS unit tests
cargo test                           # All tests on other platforms
cargo test -- --nocapture            # With stdout
```

On macOS, always run the complete unit suite with
`cargo test --workspace --exclude chelix-embedding-service`. Do not run package-specific or
name-filtered Rust unit-test commands: keep the Cargo feature graph and build cache stable across
iterations. The native embedding sidecar is built and validated separately.

### E2E Tests (Web UI)

**Every web UI change needs E2E tests.** Tests in `crates/web/ui/e2e/specs/` using Playwright.
Helpers in `e2e/helpers.js`.

```bash
cd crates/web/ui
npx playwright test                              # All
npx playwright test e2e/specs/chat-input.spec.js # Specific
```

Rules: use `getByRole()`/`getByText({ exact: true })` selectors, shared helpers
(`navigateAndWait`, `waitForWsConnected`, `watchPageErrors`), assert no JS errors,
avoid `waitForTimeout()`.

**Flaky tests must be fixed, never skipped or ignored.** If a test fails intermittently,
find and fix the root cause (race conditions, `requestAnimationFrame` timing, missing
waits, element detachment from re-renders). Do not use `test.skip()`, `test.fixme()`,
or retry-count workarounds to hide flakiness.

## Code Quality

- Never run `cargo fmt` on stable in this repo. Always select the pinned nightly explicitly with `cargo +nightly-2025-12-27 fmt --all` (add `-- --check` for check-only validation).

```bash
cargo +nightly-2025-12-27 fmt --all              # Format Rust
cargo +nightly-2025-12-27 fmt --all -- --check   # Check Rust formatting
just release-preflight   # fmt + clippy gates
cargo check              # Fast compile check
taplo fmt                # Format TOML files
biome check --write      # Lint/format TS/TSX
```

## Sandbox Architecture

Containers (Docker or Apple Container) in `crates/tools/src/sandbox.rs` (trait + impls),
`command.rs` (shared command execution), `execute_command` tooling,
`crates/cli/src/sandbox_commands.rs` (CLI), `crates/config/src/schema.rs` (config).

Pre-built images use deterministic hash tags from base image + packages. Default packages
in `default_sandbox_packages()`. CLI: `chelix sandbox {list,build,remove,clean}`.

## Logging Levels

- `error!` — unrecoverable. `warn!` — unexpected but recoverable. `info!` — operational milestones.
- `debug!` — detailed diagnostics. `trace!` — very verbose per-item data.
- **Common mistake:** `warn!` for unconfigured providers — use `debug!` for expected "not configured" states.

## Security

- **WebSocket Origin validation**: `server.rs` rejects cross-origin WS upgrades (403). Loopback variants equivalent.
- **SSRF protection**: `chelix-common` blocks loopback/private/link-local/CGNAT IPs. Preserve this on changes.
- **Secrets**: Use `secrecy::Secret<String>` for all passwords/keys/tokens. `expose_secret()` only at consumption point. Manual `Debug` impl with `[REDACTED]`. Scope `RwLock` read guards in blocks to avoid deadlocks. See `crates/oauth/src/types.rs` for serde helpers.
- **Never commit** passwords, credentials, `.env` with real values, or PII.
- If secrets accidentally committed: `git reset HEAD~1`, remove, re-commit. If pushed, rotate immediately.

## Data and Config Directories

- **Config**: `chelix_config::config_dir()` (`~/.chelix/`). Contains `chelix.toml`, `credentials.json`, `mcp-servers.json`.
- **Data**: `chelix_config::data_dir()` (`~/.chelix/`). Contains DBs, sessions, logs, memory files.
- **Never** use `directories::BaseDirs` outside `chelix-config`. Never use `std::env::current_dir()` for storage.
- Workspace-scoped files (`MEMORY.md`, `memory/*.md`, etc.) resolve relative to `data_dir()`.
- Gateway resolves `data_dir` once at startup; prefer that value over repeated calls.

## Database Migrations

sqlx migrations, each crate owns its `migrations/` directory. See `docs/sqlite-migration.md`.

| Crate | Tables |
|-------|--------|
| `chelix-projects` | `projects` |
| `chelix-sessions` | `sessions`, `channel_sessions` |
| `chelix-cron` | `cron_jobs`, `cron_runs` |
| `chelix-gateway` | `auth_*`, `passkeys`, `api_keys`, `env_variables`, `message_log`, `channels` |
| `chelix-memory` | `files`, `chunks`, `embedding_cache`, `chunks_fts` |

New migration: `crates/<crate>/migrations/YYYYMMDDHHMMSS_description.sql` (use `IF NOT EXISTS`).
New crate: add `run_migrations()` to `lib.rs`, call from `server.rs` in dependency order.

## Provider Implementation

- **Async all the way down** — never `block_on` in async context. All HTTP/IO must be async.
- Make model lists broad (API errors handle unavailable models). Check `../clawdbot/` for reference.
- BYOM providers (OpenRouter, Ollama): require user config, don't hardcode models.

## Changelog

- Do **not** add manual `CHANGELOG.md` entries in normal PRs.
- `CHANGELOG.md` entries are generated from commit history via `git-cliff` (`cliff.toml`).
- Use conventional commits and preview unreleased notes with `just changelog-unreleased`.
- PR CI enforces this via `scripts/check-changelog-guard.sh`.

## Git Workflow

Conventional commits: `feat|fix|docs|style|refactor|test|chore(scope): description`
- Prefer descriptive commit subjects over terse "change stuff" summaries.
- For bug fixes, behavioral changes, and non-obvious refactors, include a commit body that explains the concrete problem, the root cause, and why the chosen fix is correct.
- Write commit messages so `git log` is useful without opening the diff first.
**No `Co-Authored-By` trailers.** Update `README.md` features list with `feat` commits.

### Releases

- Date-based versioning: `YYYYMMDD.NN` (e.g., `20260311.01`). Cargo.toml stays at static `0.1.0`; real version injected via `CHELIX_VERSION` env var at build time.
- Never overwrite tags — always create new version.
- Use `./scripts/prepare-release.sh [YYYYMMDD.NN]` for release prep (auto-computes next version if omitted).
- Deploy template tags updated automatically by CI — don't manually update.

**Release workflow is two phases:**

1. **Prepare & publish** (can be done in a session):
   ```bash
   ./scripts/prepare-release.sh          # generates changelog, syncs lockfile
   git add -A && git commit -m "chore: prepare release YYYYMMDD.NN"
   git tag YYYYMMDD.NN && git push --follow-tags
   ```
   CI then builds artifacts, generates checksums, Sigstore signatures, and creates the GitHub release. This takes time.

2. **GPG-sign** (must happen later, after CI completes):
   ```bash
   ./scripts/gpg-sign-release.sh [VERSION]
   ```
   This downloads artifacts from the published release, verifies SHA256 checksums, signs each artifact with the maintainer's YubiKey-resident GPG key, and uploads `.asc` files back to the release. **Requires YubiKey tap.**

   Users verify signatures with:
   ```bash
   ./scripts/verify-release.sh --version YYYYMMDD.NN
   ```

**Important:** When asked to create a release, complete phase 1 and remind the maintainer to run `gpg-sign-release.sh` after CI finishes. Do not attempt to run the signing script in the same session — the release artifacts won't exist yet.

### Lockfile

- `cargo fetch` to sync (not `cargo update`). Verify with `cargo fetch --locked`. `local-validate.sh` auto-handles.
- `cargo update --workspace` only for intentional upgrades.

### Local Validation

**Always** run `./scripts/local-validate.sh <PR_NUMBER>` when a PR exists.

For incremental local edits before full validation:
- TS/TSX changed: run `biome check --write` and `cd crates/web/ui && npm run build`.
- Rust changed: run `cargo +nightly-2025-12-27 fmt --all -- --check`.
- Both changed: run all three.

Exact commands (must match `local-validate.sh`):
- Fmt: `cargo +nightly-2025-12-27 fmt --all -- --check`
- Clippy: `just lint` (OS-aware: on macOS excludes CUDA features, on Linux uses `--all-features`)
- Tests: `just test` (OS-aware: on macOS uses nextest without CUDA features, on Linux uses `--all-features`)

### PR Descriptions

Required sections: `## Summary`, `## Validation` (checkboxes, split into `### Completed` / `### Remaining`
with exact commands), `## Manual QA`. Include concrete test steps.
- Do not prefix GitHub PR titles with `[codex]`.
- Prefer normal human-readable PR titles, ideally aligned with the conventional-commit summary.

## Code Quality Checklist

**Run before every commit:**
- [ ] No secrets or private tokens (CRITICAL)
- [ ] `taplo fmt` (TOML changes)
- [ ] `biome check --write` (TS/TSX changes)
- [ ] Rust fmt passes (exact command above)
- [ ] `just lint` passes (OS-aware clippy)
- [ ] `just release-preflight` passes
- [ ] `just test` passes
- [ ] Conventional commit message
- [ ] No debug code or temp files

## Documentation

Source in `docs/src/` (mdBook).
Update `docs/src/SUMMARY.md` when adding pages. Preview: `cd docs && mdbook serve`.

**Keep docs in sync with code.** When adding or changing user-facing features
(config fields, CLI commands, channel behavior, API endpoints, tools), update
the relevant `docs/src/` pages and the config template (`crates/config/src/template.rs`)
in the same PR. Documentation drift causes real user confusion — treat outdated
docs as a bug.

## Session Completion

**Work is NOT complete until `git push` succeeds.** Mandatory steps:
1. File issues for remaining work
2. Run quality gates
3. Update issue status
4. **Push**: `git pull --rebase && git push && git status`
5. Clean up stashes/branches
6. Hand off context

## Plans and Session History

Plans in `prompts/`. After significant work, write summary to
`prompts/session-YYYY-MM-DD-<topic>.md`.
