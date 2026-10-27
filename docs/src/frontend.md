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
│   │   ├── tool-permission.ts   # Session auto/moderated toolbar, tool-card gates, deny-feedback dialog
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
│   ├── vite.config.ts           # Vite build configuration
│   ├── tsconfig.json            # TypeScript strict config
│   └── package.json             # Dependencies & scripts
├── src/
│   ├── assets/                  # Served static assets
│   │   ├── dist/                # Generated Vite build output (ignored)
│   │   ├── css/                 # Stylesheets (Tailwind + custom)
│   │   ├── js/                  # Share page
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

## Session Sidebar

`components/SessionList.tsx` renders each session with a title and a summary of
up to two lines each. The title uses one line when its text, message-count badge
and time fit together; otherwise it uses two lines. The status icon and archive
mark sit on the first title line; the count badge and time occupy the right end
of the last title line. Both title lines start at the same position. The count
badge is 0.7rem tall, matching the time's font size, with 8.5px digits. The
title-to-count and count-to-time gaps are 2px. The agent chip sits at the start
of the summary's first line, followed by the preview and fork/worktree/project
metadata. The second summary line starts beneath the chip. Title and summary
text wrap at character boundaries and are truncated with an ellipsis at the end
of the available second line, with full titles, previews and metadata available
on hover.
Times use the 24-hour `h23` cycle, including `00:16` at midnight.
Icons identify the session type (cron, channel or chat) at every tree level.
Rows have 2px of left padding after the 3px selection border, with a 2px gap
between the icon and title. The summary aligns with the title.

Session row heights follow their content, with 4px vertical padding on desktop
and 5px on mobile. Each child level adds
12px of indentation: the guide starts at the parent icon's centre, with a 4px
elbow reaching the child icon. The 1px guides use `#5a606b`; the last child's
incoming guide ends at its elbow. Ancestor guides continue through nested
branches, and a divider appears after the final descendant of each root group.

The search field and new-session button are 28px tall. The **Archive** checkbox
sits beside the Sessions and Cron tabs and is shown on the Sessions tab. Panel
styles are defined in `src/assets/css/layout.css`, with the mobile row spacing
in `src/assets/css/mobile.css`.

## Provider Segment and Keyed Rendering

`UiHistoryEngine` supplies semantic snapshots to both history pages and live
subscriptions. The frontend contract is in `src/types/ui-history.ts`.

- **Message identity**: `id`, immutable `position`, and `revision` identify each
  snapshot within a session `generation`.
- **Provider items**: segment/item IDs and item positions originate in Rust;
  `provider-segment-reducer.ts` renders reasoning from those ordered items.
- **One disclosure per segment**: `session-render.ts` keeps a retained
  assistant's reasoning disclosure while updating its text body. Every `active`
  revision renders it open with the `Thinking` summary; manually closing it while
  active lasts only until the next revision. The first non-active render, whether
  it follows an active revision or creates a node from a terminal baseline,
  renders it closed with the `Reasoning` summary. Later non-active revisions
  preserve the user's current open state while the keyed DOM node is retained. A
  session or generation reset creates a new node and reapplies the baseline rule.
  Non-active covers every terminal segment outcome: `completed`, `incomplete`,
  `failed`, `cancelled`, and `transport_error`.
- **Keyed DOM**: each history container has `data-message-id`; reconciliation
  orders containers by snapshot position and updates only newer revisions.
- **Authoritative counts**: accepted pages and batches supply `totalMessages`.
  Text and voice pending sends have UUID correlation and separate pending DOM.

`sessions/history-subscription.ts` obtains a subscribed snapshot baseline and
buffers early `ui_history` events. Generation changes replace the window;
revision gaps request another baseline. The server isolates slow listeners in
per-client tasks and can send a replacement snapshot of the selected range.

The status-line context-budget percentage is driven directly by received tool
lifecycle snapshots. An accepted live batch applies the newest budget source by
`(position, revision)` before history reconciliation. A page baseline contributes
a budget source only when it reaches the session tail (`hasNewer === false`), so
an older pagination or search range cannot replace a newer value. Budget metadata
from an early delta remains in memory across baseline buffering and a replacement
subscription caused by buffer overflow. Switching sessions clears the previous
percentage. Percentage calculation follows
[Context-budget metadata](compaction.md#context-budget-metadata).

`stores/session-history-cache.ts` keeps one in-memory session window, bounded
by 240 messages and 12 MiB of serialized snapshots with a one-message minimum.
Bidirectional pagination evicts the opposite edge and preserves the reading
viewport. Session selection, history, composer recall, and search context are
held in memory; server session lists determine session availability. Search
loads the exact hit's ID and generation before highlighting it.

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

`src/tool-lifecycle.ts` validates the lifecycle contract. The server engine
accumulates input fragments and sends `accumulatedArguments` alongside the
latest lifecycle stage. Snapshot metadata includes `runId`, `assistantId`,
and received context budget. Reconnecting during argument
streaming uses this same accumulated snapshot.

One rendering path in `src/ws/tool-helpers.ts` applies those snapshots to tool
cards:

- `created` creates the live invocation bubble before arguments are complete;
- `input_streaming` updates the displayed accumulated JSON input;
- `input_ready` replaces it with decoded arguments; `assistantId` links the
  tool snapshot to its owning assistant;
- waiting, execution, and progress stages update the same card;
- terminal stages render success, rejection, failure, or cancellation; lifecycle
  results remain strings on the wire and are JSON-decoded only when structured
  presentation needs object fields.

`src/tool-permission.ts` puts approve, deny, and skip on the active invocation
card while a moderated/manual permission is pending. Deny opens the Deny tool
call dialog. The feedback field fills the height between the title and the
CANCEL/SEND row and shows its scrollbar when the text is taller than that area.
At widths up to 767px the dialog occupies a full-width bottom sheet, like the
session Terminal window. SEND calls `tool.permission.resolve` with decision
`deny` and feedback equal to the field value after trimming the ends. That trim
keeps internal line breaks. SEND stays disabled while the trim is empty. CANCEL
and a backdrop click close the dialog without a decision. The raw field value is
kept in one UI variable until that same value is sent successfully or the active
session key changes. A field length of 0 leaves that variable unset. Opening the
dialog again with the same active session key copies the saved value back into
the field. Changing the active session key, including leaving no session
selected, drops the variable and closes the dialog. A failed SEND leaves the
open dialog, the field text, and the variable. The card shows the
denial text with those line breaks.

Execution progress is backend-authored. `src/ws/tool-helpers.ts` uses the
snapshot's lifecycle stage for tool rendering and terminal attachment.

History loading and live revisions share `session-render.ts`. A tool's
`presentation.document` can replace its ordinary card with text, Markdown, or
a diff. The backend lifecycle hook `AgentTool::ui_presentation` supplies this
representation separately from provider arguments and results. Checkpoint and
error presentations use the same addressable presentation contract.

`ws/chat-handlers.ts` updates run, queue, voice, and compaction status.
Conversation content and counts are applied by the semantic history reducer.

## Shared Component Library

Reusable components in `components/forms/`:

- **Form fields**: `TextField`, `TextAreaField`, `SelectField`, `CheckboxField`
- **Layout**: `SectionHeading`, `SubHeading`, `SettingsCard`, `DangerZone`
- **Lists**: `ListItem`, `Badge`, `EmptyState`, `Loading`, `CopyButton`
- **Navigation**: `TabBar`
- **State**: `useSaveState()` hook, `SaveButton`, `StatusMessage`

## Asset Serving

The Rust `chelix-web` crate serves UI assets only from bytes embedded
by `include_dir!`. A missing asset is a 404.

HTML templates are rendered by [Askama](https://github.com/djc/askama) with
server-injected data (`window.__CHELIX__`, the "gon" pattern).

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
```

For CSS changes, also run `npm run build:css` and commit `style.css`.
