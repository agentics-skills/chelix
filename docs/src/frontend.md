# Frontend Architecture

The chelix web UI is a TypeScript single-page application built with
[Preact](https://preactjs.com/) and [Vite](https://vite.dev/).

## Directory Layout

```
crates/web/
├── ui/                          # TypeScript source & tooling
│   ├── src/                     # Application source
│   │   ├── app.tsx              # Main entry point
│   │   ├── tool-lifecycle.ts    # Tool invocation reducer and wire guards
│   │   ├── a2ui-renderer.ts     # Official A2UI Lit chat renderer
│   │   ├── login-app.tsx        # Login page entry
│   │   ├── onboarding-app.tsx   # Onboarding wizard entry
│   │   ├── types/               # Shared type definitions
│   │   ├── stores/              # Preact Signal stores
│   │   ├── components/          # Reusable Preact components
│   │   │   └── forms/           # Form field & layout components
│   │   ├── pages/               # Page components
│   │   │   ├── sections/        # Settings page sections
│   │   │   ├── channels/        # Channel modal sub-components
│   │   │   └── chat/            # Chat page sub-modules
│   │   ├── providers/           # Provider setup sub-modules
│   │   ├── sessions/            # Session management sub-modules
│   │   ├── onboarding/          # Onboarding step components
│   │   ├── ws/                  # WebSocket handler sub-modules
│   │   ├── hooks/               # Custom Preact hooks
│   │   └── locales/             # i18n translations (en, fr, zh)
│   ├── e2e/                     # Playwright E2E tests
│   ├── vite.config.ts           # Vite build configuration
│   ├── tsconfig.json            # TypeScript strict config
│   └── package.json             # Dependencies & scripts
├── src/
│   ├── assets/                  # Served static assets
│   │   ├── dist/                # Generated Vite build output (ignored)
│   │   ├── css/                 # Stylesheets (Tailwind + custom)
│   │   ├── js/                  # E2E test shims + share page
│   │   ├── icons/               # Favicons & PWA icons
│   │   └── sw.js                # Service worker
│   └── templates/               # Askama HTML templates
```

## Build Pipeline

### TypeScript → JavaScript (Vite)

Source files in `ui/src/` are compiled and bundled by Vite into
`src/assets/dist/`. Three entry points produce three bundles:

- `dist/main.js` — main app (chat, settings, all pages)
- `dist/login.js` — login page
- `dist/onboarding.js` — onboarding wizard

```bash
cd crates/web/ui
npm run build          # Production build → ../src/assets/dist/
npm run dev            # Watch mode (rebuilds on file changes)
```

The generated `dist/` output is ignored by Git. Run the production build before
packaging or serving changes to the TypeScript frontend.

### CSS (Tailwind)

Tailwind CSS is built separately from the TypeScript pipeline:

```bash
cd crates/web/ui
npm run build:css      # input.css → ../src/assets/css/style.css
npm run watch:css      # Watch mode
```

The output `style.css` is committed unminified (one rule per line) so diffs
merge cleanly.

### Service Worker

The service worker is built from TypeScript via esbuild:

```bash
cd crates/web/ui
npm run build:sw       # src/sw.ts → ../src/assets/sw.js
```

### Full Build

```bash
cd crates/web/ui
npm run build:all      # Vite + Tailwind + service worker
```

## Technology Stack

| Layer               | Technology                                                      |
| ------------------- | --------------------------------------------------------------- |
| UI framework        | [Preact](https://preactjs.com/) (lightweight React alternative) |
| Generative UI       | [A2UI](https://a2ui.org/) v0.9.1 with the official Lit renderer |
| Templating          | JSX with typed Props interfaces                                 |
| State management    | [Preact Signals](https://preactjs.com/guide/v10/signals/)       |
| Build tool          | [Vite](https://vite.dev/) with `@preact/preset-vite`            |
| Type checking       | TypeScript strict mode (`tsc --noEmit`)                         |
| Linting/formatting  | [Biome](https://biomejs.dev/)                                   |
| CSS                 | [Tailwind CSS](https://tailwindcss.com/) v4                     |
| i18n                | [i18next](https://www.i18next.com/) (en, fr, zh)                |
| Charts              | [uPlot](https://github.com/leeoniya/uPlot)                      |
| Terminal            | [xterm.js](https://xtermjs.org/)                                |
| Syntax highlighting | [Shiki](https://shiki.style/) (bundled, lazy-loaded)            |
| E2E testing         | [Playwright](https://playwright.dev/)                           |

## Provider Segment and Keyed Rendering

Chat streaming and history reload use a single typed materializer in
`src/sessions/provider-segment-reducer.ts`.

- **Segment ID**: identifies a provider attempt/response.
- **Item ID & Position**: assigned once on provider ingress in Rust and carried
  on every update. The reducer orders items by that position and never assigns
  one of its own.
- **Reasoning Parts**: provider part index orders structured summary chunks.
- **One Disclosure per Bubble**: all reasoning items of a segment render as parts
  of a single disclosure, in the live view and after a reload alike.
- **Cache Indexing**: cache updates key on physical `historyIndex` and item identity rather than transient `run_id`.
- **No DOM Reordering**: DOM insertion time does not determine reasoning position.

## Type Safety

The codebase enforces strict TypeScript with zero tolerance for `any`:

- **`tsc --noEmit`** runs in CI and local-validate (must pass with 0 errors)
- **107 typed RPC methods** via `RpcMethodMap` — calling
  `sendRpc("models.list", {})` infers the response type as `ModelInfo[]`
- **28 WebSocket events** via `WsEventName` enum with typed payload
  discriminated unions
- **`ChannelType` enum** for channel type comparisons (no raw strings)
- **`targetValue(e)` / `targetChecked(e)`** helpers eliminate
  `(e.target as HTMLInputElement).value` casts

## Tool invocation lifecycle

Tool invocations use one discriminated WebSocket and history contract declared
in `src/types/ws-events.ts`. Every event has `toolCallId`, `toolName`, `sequence`,
`emittedAtMs`, and one of these stages:

```text
created
input_streaming
input_ready
waiting_for_execution
executing
execution_progress
result_ready
completed
rejected
cancelled
```

`src/tool-lifecycle.ts` validates that contract and reduces each call to its
latest snapshot. Events with a lower sequence than the current snapshot cannot
roll the invocation backward. `input_streaming` events carry one delta; the
snapshot keeps their accumulated text separately. The snapshot also carries
transport metadata such as `runId`, a physical history index when replaying a
persisted snapshot, the assistant message index assigned at `input_ready`,
execution mode, and context budget.

One rendering path in `src/ws/tool-helpers.ts` applies those snapshots to tool
cards:

- `created` creates the live invocation bubble before arguments are complete;
- `input_streaming` updates the displayed accumulated JSON input;
- `input_ready` replaces it with decoded arguments and binds the live assistant
  segment to the separately persisted canonical assistant frame;
- waiting, execution, and progress stages update the same card;
- terminal stages render success, rejection, failure, or cancellation; lifecycle
  results remain strings on the wire and are JSON-decoded only when structured
  presentation needs object fields.

Execution progress is backend-authored. During interactive live rendering, the
`execute_command` component displays the lifecycle message and attaches its
terminal only after an `execution_progress` event reports `elapsedMs >= 10000`;
it does not run a local elapsed-time clock. Persisted history rendering is
non-interactive and never attaches a terminal.

Live WebSocket events, append-only session history, and reconnect snapshots all
reuse this reducer and renderer. The backend history projection keeps only the
latest `role: "tool_lifecycle"` record for each `(runId, toolCallId)` invocation,
and the frontend cache uses the same identity so lifecycle transitions replace
one logical entity and increment the session count only once.
`src/sessions/session-switch.ts` restores the latest non-terminal snapshots from
`activeToolInvocations`.

## Shared Component Library

Reusable components in `components/forms/`:

- **Form fields**: `TextField`, `TextAreaField`, `SelectField`, `CheckboxField`
- **Layout**: `SectionHeading`, `SubHeading`, `SettingsCard`, `DangerZone`
- **Lists**: `ListItem`, `Badge`, `EmptyState`, `Loading`, `CopyButton`
- **Navigation**: `TabBar`
- **State**: `useSaveState()` hook, `SaveButton`, `StatusMessage`

## Asset Serving

The Rust `chelix-web` crate serves assets with three-tier resolution:

1. **Dev filesystem** — `CHELIX_ASSETS_DIR` env var or auto-detected from the
   crate source tree (`cargo run` dev mode)
2. **External share dir** — `share_dir()/web/` for packaged deployments
3. **Embedded fallback** — `include_dir!` compiled into the binary

HTML templates are rendered by [Askama](https://github.com/djc/askama) with
server-injected data (`window.__CHELIX__`, the "gon" pattern).

## E2E Test Compatibility

E2E tests dynamically import individual JS modules (e.g.,
`await import("js/state.js")`) to inspect and mock internal app state. With Vite
bundling, individual modules don't exist as standalone files.

**Shim layer**: small proxy files in `src/assets/js/` re-export from
`window.__chelix_modules` (populated by `app.tsx` at startup). This lets tests
import modules at their original paths without changes.

The shims are only loaded by E2E tests, never by the production app.

## A2UI chat surfaces

`src/a2ui-renderer.ts` owns the official A2UI Lit `MessageProcessor`, surface
mounting, standard action validation, and card lifecycle. Live tool events use
the same module as persisted history reconstruction. Active surfaces submit
actions through the typed `a2ui.action` RPC; completed and restored surfaces
are read-only.

See [Generative UI with A2UI](a2ui.md) for the supported protocol profile,
agent tool contract, routing checks, persistence, and troubleshooting.

## Development Workflow

After changing TypeScript source files:

```bash
cd crates/web/ui

# 1. Type check
npx tsc --noEmit

# 2. Lint and format
biome check --write src/

# 3. Build (commits dist/ output)
npm run build

# 4. Run E2E tests
npx playwright test --project default
```

For CSS changes, also run `npm run build:css` and commit `style.css`.
