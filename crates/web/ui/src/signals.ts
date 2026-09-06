// ── Preact signal bridge for shared state ─────────────────────
// Mirrors shared state.ts values as Preact signals for imperative
// code (websocket.ts) and Preact pages.
//
// Signals for models, projects, sessions, selectedModelId, and
// activeSessionKey are owned by stores/*.ts and re-exported by signals.ts.

import type { Signal } from "@preact/signals";
import { signal } from "@preact/signals";
import { models, selectedModelId } from "./stores/model-store";
import { projects } from "./stores/project-store";
import { activeSessionKey, sessions } from "./stores/session-store";
import type { SandboxGonInfo } from "./types/gon";

export { activeSessionKey, models, projects, selectedModelId, sessions };

// Shared UI signals
export const connected: Signal<boolean> = signal(false);
export const cachedChannels: Signal<unknown | null> = signal(null);
export const unseenErrors: Signal<number> = signal(0);
export const unseenWarns: Signal<number> = signal(0);
export const sandboxInfo: Signal<SandboxGonInfo | null> = signal(null);
